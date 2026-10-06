using System;
using System.Collections.Generic;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Controls;

/// <summary>
/// Compose bar for DM conversations with attachment support,
/// reply preview, optional subject line, and markdown toolbar.
///
/// The body field (<c>dm-text-field</c>) is a <see cref="MarkdownRichEditBox"/> used as
/// an in-place markdown decoration surface over the literal markdown source — the buffer
/// holds the raw markdown (markers present) and <see cref="MessageText"/> returns it
/// verbatim on send (conversations.md § Compose-field inline markdown styling). The
/// inline styling (CharacterFormat over source ranges) is applied in
/// <c>DmComposeBar.Decoration.cs</c>.
/// </summary>
public sealed partial class DmComposeBar : UserControl
{
    // Whether compose.attachments (rendered via SetAttachments) is currently
    // non-empty — lets SendButton_Click allow an attachment-only send (empty
    // body) without the control holding attachment bytes itself; the shared
    // manager is the only holder of staged-attachment state now (conversations.md
    // § Attachments).
    private bool _hasAttachments;

    // Set around a programmatic MessageText set so the RichEditBox TextChanged it
    // raises does NOT fire BodyChanged (a seed must not look like user input).
    private bool _suppressBodyChanged;

    // The last plain-text body BodyChanged was raised for (or seeded with). The
    // RichEditBox re-fires TextChanged on a re-render / markdown-decoration pass with
    // IDENTICAL plain text (no user edit), so BodyChanged is value-idempotent: a
    // TextChanged that does not change the plain text is swallowed. Without this, a
    // snapshot-render tick re-fired BodyChanged → the page's debounced draft save was
    // reset faster than its window, so the nest __drafts PUT never landed
    // (test_conversations_draft_persistence; conversations.md § Persistence).
    private string _lastBodyEmitted = string.Empty;

    /// <summary>Raised when user clicks Send. Arg: the compose body. Any staged
    /// attachment is already in the shared manager via <see cref="AttachmentPicked"/>,
    /// so the page's send just routes the body — no attachment payload to carry here.</summary>
    public event Func<string, Task>? SendRequested;

    /// <summary>Raised when the user picks a file via <c>attachment-button</c>, after
    /// EXIF-stripping. Args: (filename, mimeType, bytes). The page stages it
    /// immediately via the shared <c>add_attachment</c>/<c>add_new_thread_attachment</c>
    /// (conversations.md § Attachments) — not deferred to Send — so it renders in
    /// the chip row (<see cref="SetAttachments"/>) right away.</summary>
    public event Action<string, string, byte[]>? AttachmentPicked;

    /// <summary>Raised when a staged-attachment chip's × is clicked. Arg: the
    /// chip's index in the current <c>compose.attachments</c> list (the page maps
    /// it to <c>remove_attachment</c>/<c>remove_new_thread_attachment</c>).</summary>
    public event Action<int>? RemoveAttachmentRequested;

    /// <summary>Raised when user cancels a reply preview.</summary>
    public event Action? ReplyCancelled;

    /// <summary>Raised when the topic-toggle-button is toggled. True = expanded.</summary>
    public event Action<bool>? TopicToggled;

    /// <summary>Raised when the subject-input text changes.</summary>
    public event Action<string>? SubjectChanged;

    /// <summary>Raised when the compose BODY text changes by user input (typing or the
    /// e2e ValuePattern), carrying the literal markdown source. NOT raised for
    /// programmatic seeds via the <see cref="MessageText"/> setter (draft restore /
    /// clear-after-send) — those are suppressed so a seed never re-triggers a save.
    /// The page forwards it to the shared manager + schedules a debounced
    /// <c>__drafts</c> persist (conversations.md § Persistence).</summary>
    public event Action<string>? BodyChanged;

    /// <summary>Raised when a reply-recipient chip's × is clicked. Arg: the
    /// chip's index in the current reply-recipient list (the page maps it to
    /// the shared <c>compose.reply_recipients[i]</c> → <c>remove_reply_recipient</c>).</summary>
    public event Action<int>? RemoveReplyRecipientRequested;

    /// <summary>Raised with the raw typed text when the add-recipient box is
    /// committed (Enter). The page parses it via the shared
    /// <c>try_parse_typed_address</c> and calls <c>add_reply_recipient</c>.</summary>
    public event Action<string>? AddReplyRecipientRequested;

    /// <summary>Gets or sets the current compose text — the literal markdown source.</summary>
    public string MessageText
    {
        get => MessageBox.GetPlainText();
        set
        {
            _suppressBodyChanged = true;
            try { MessageBox.SetPlainText(value); _lastBodyEmitted = value ?? string.Empty; }
            finally { _suppressBodyChanged = false; }
        }
    }

    /// <summary>Gets or sets the current subject text. A set is a SEED, silent like
    /// <see cref="MessageText"/>'s: it raises no <see cref="SubjectChanged"/> — see
    /// <see cref="_lastSubjectEmitted"/>.</summary>
    public string SubjectText
    {
        get => SubjectInputBox.Text;
        set
        {
            _lastSubjectEmitted = value ?? string.Empty;
            SubjectInputBox.Text = value;
        }
    }

    /// <summary>Whether the topic toggle is currently expanded. A set is a SEED: it
    /// raises no <see cref="TopicToggled"/> — see <see cref="_lastSubjectEmitted"/>.</summary>
    public bool TopicExpanded
    {
        get => TopicToggleButton.IsChecked == true;
        set
        {
            _lastTopicEmitted = value;
            TopicToggleButton.IsChecked = value;
        }
    }

    /// <summary>
    /// The subject and topic state the manager already knows — seeded by the page,
    /// or last forwarded from a user edit. An event carrying that same value is not
    /// forwarded again, the same idempotence <see cref="_lastBodyEmitted"/> gives
    /// the body. By VALUE, not a flag held across the set: WinUI raises
    /// <c>TextBox.TextChanged</c> asynchronously, after the setter has returned.
    /// <para>Load-bearing, not tidiness: the page re-seeds both on every observer
    /// tick, and the detail bar's <see cref="TopicToggled"/> handler calls the
    /// manager's <c>toggle_topic</c> — a FLIP, not a set. A seed forwarded as an
    /// edit therefore flipped the manager's subject to the opposite state, whose
    /// notify re-seeded the checkbox the other way, and so on: an endless refresh
    /// loop on the UI thread. Measured on windows after a refused send left a
    /// subject draft on a freshly materialized thread — ~1,000 refreshes a second
    /// for the rest of the process, which starved UI Automation so thoroughly that
    /// the next sign-in's Status page read as absent.</para>
    /// </summary>
    private string _lastSubjectEmitted = string.Empty;

    /// <summary>The topic half of <see cref="_lastSubjectEmitted"/>.</summary>
    private bool _lastTopicEmitted;

    public DmComposeBar()
    {
        this.InitializeComponent();
        MessageBox.PlaceholderText = S.Get("groups/message_placeholder");
        SendButton.Content = S.Get("common/send");
        ReplyToLabel.Text = S.Get("conversations/unified/to_line_label");
        ReplyRecipientAddBox.PlaceholderText = S.Get("conversations/unified/reply_recipient_add_placeholder");
        // The markdown toolbar wraps/​prefixes over the compose field through the
        // IMarkdownEditTarget seam (MarkdownRichEditBox implements it).
        Toolbar.Target = MessageBox;
        // The markdown-marker-toggle-button flips this editor's inline-marker visibility
        // (hide-by-default; conversations.md § Compose-field inline markdown styling). The
        // decoration applier owns the per-editor state.
        Toolbar.MarkersShownChanged += SetMarkersShown;
        // Live body-change signal for draft persistence v2 (conversations.md
        // § Persistence). SetPlainText raises TextChanged synchronously, so the
        // _suppressBodyChanged guard set by the MessageText setter keeps a seed silent;
        // user typing and the e2e ValuePattern (which bypasses the setter) both fire it.
        MessageBox.TextChanged += (_, _) =>
        {
            if (_suppressBodyChanged) return;
            var text = MessageText;
            // Idempotent: the RichEditBox re-fires TextChanged on a re-render/decoration
            // pass with unchanged plain text — that is not a user edit, so do not raise
            // BodyChanged (which would reset the page's debounced draft save in a loop).
            if (text == _lastBodyEmitted) return;
            _lastBodyEmitted = text;
            BodyChanged?.Invoke(text);
        };
        InitializeDecoration();
    }

    /// <summary>
    /// Apply per-thread capability gating to compose-bar affordances.
    /// Every element renders unconditionally; this method only flips
    /// <c>IsEnabled</c>. Tests assert against the <c>disabled</c> attribute,
    /// never the rail directly (spec section 4, § Capability gating).
    /// </summary>
    internal void SetCapabilities(uniffi.fauna_conversations.ThreadCapabilities caps)
    {
        AttachButton.IsEnabled = caps.supportsAttachments;
        TopicToggleButton.IsEnabled = caps.supportsSubject;
        Toolbar.SetToolbarEnabled(caps.supportsMarkdown);
        SetDecorationEnabled(caps.supportsMarkdown);
        // Editable reply "To" line is shown only on recipient-selection rails
        // (mail). Collapsed → no UIA peer, so the e2e counts 0 off mail
        // (conversations.md § Participants vs. reply recipients).
        ReplyToLine.Visibility = caps.supportsRecipientSelection
            ? Visibility.Visible
            : Visibility.Collapsed;
    }

    /// <summary>
    /// Rebuild the editable reply "To" line chip row from the shared
    /// <c>compose.reply_recipients</c> (display strings, pre-resolved by the
    /// page). Each chip carries the indexed <c>dm-reply-recipient-chip</c> id on
    /// its inner TextBlock (UIA-collapse lesson) and a × (<c>dm-reply-recipient-remove</c>)
    /// that drops that recipient from this reply only. Built imperatively
    /// (mirrors linux <c>compose_bar.rs</c>); the panel is non-virtualizing so
    /// the e2e count-all is bounded.
    /// </summary>
    public void SetReplyRecipients(IReadOnlyList<string> displays)
    {
        ReplyRecipientChips.Children.Clear();
        for (int i = 0; i < displays.Count; i++)
        {
            int index = i; // capture for the remove handler
            var chipText = new TextBlock
            {
                Text = displays[i],
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(8, 2, 0, 2),
                FontSize = 12,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(
                chipText, Ids.DmReplyRecipientChip);

            var removeBtn = new Button
            {
                Content = "",
                FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Segoe MDL2 Assets"),
                FontSize = 10,
                Padding = new Thickness(4, 2, 6, 2),
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(
                removeBtn, Ids.DmReplyRecipientRemove);
            removeBtn.Click += (_, _) => RemoveReplyRecipientRequested?.Invoke(index);

            var inner = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 2 };
            inner.Children.Add(chipText);
            inner.Children.Add(removeBtn);

            var chip = new Border
            {
                CornerRadius = new CornerRadius(10),
                BorderThickness = new Thickness(1),
                BorderBrush = new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.Gray),
                Child = inner,
            };
            ReplyRecipientChips.Children.Add(chip);
        }
    }

    /// <summary>
    /// Rebuild the staged-attachment chip row from the shared
    /// <c>compose.attachments</c> (conversations.md § Attachments →
    /// Staged-attachment preview). Each chip carries the indexed
    /// <c>dm-compose-attachment-chip</c> id on its inner TextBlock (UIA-collapse
    /// lesson, same idiom as <see cref="SetReplyRecipients"/>) naming the file +
    /// its size (through the shared <see cref="ValueFormat.ByteSize"/> —
    /// never a hand-rolled threshold table, value-formatting.md § Byte sizes)
    /// and a × (<c>dm-compose-attachment-remove</c>) that raises
    /// <see cref="RemoveAttachmentRequested"/>. <c>index</c> is positional over
    /// <c>compose.attachments</c> — the same list this loop walks — so the chip
    /// and its remove mutator's index cannot drift. Built imperatively (mirrors
    /// linux <c>compose_bar.rs</c> <c>build_attachment_chip</c>); the panel is
    /// non-virtualizing so the e2e count-all is bounded.
    /// </summary>
    internal void SetAttachments(IReadOnlyList<uniffi.fauna_conversations.AttachmentDraft> attachments)
    {
        AttachmentChips.Visibility = attachments.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        _hasAttachments = attachments.Count > 0;
        AttachmentChips.Children.Clear();
        for (int i = 0; i < attachments.Count; i++)
        {
            int index = i; // capture for the remove handler
            var draft = attachments[i];

            var chipText = new TextBlock
            {
                Text = $"{(draft.isImage ? "🖼" : "📄")} {draft.filename}  {ValueFormat.ByteSize(draft.sizeBytes)}",
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(8, 2, 0, 2),
                FontSize = 12,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(
                chipText, Ids.DmComposeAttachmentChip);

            var removeBtn = new Button
            {
                Content = "",
                FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Segoe MDL2 Assets"),
                FontSize = 10,
                Padding = new Thickness(4, 2, 6, 2),
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(
                removeBtn, Ids.DmComposeAttachmentRemove);
            removeBtn.Click += (_, _) => RemoveAttachmentRequested?.Invoke(index);

            var inner = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 2 };
            inner.Children.Add(chipText);
            inner.Children.Add(removeBtn);

            var chip = new Border
            {
                CornerRadius = new CornerRadius(10),
                BorderThickness = new Thickness(1),
                BorderBrush = new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.Gray),
                Child = inner,
            };
            AttachmentChips.Children.Add(chip);
        }
    }

    private void ReplyRecipientAdd_KeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        if (e.Key != Windows.System.VirtualKey.Enter) return;
        var text = ReplyRecipientAddBox.Text;
        if (!string.IsNullOrWhiteSpace(text))
        {
            AddReplyRecipientRequested?.Invoke(text);
        }
        ReplyRecipientAddBox.Text = string.Empty;
        e.Handled = true;
    }

    /// <summary>Shows the reply preview bar with the given text.</summary>
    public void ShowReplyPreview(string previewText)
    {
        ReplyPreviewText.Text = previewText;
        ReplyPreview.Visibility = Visibility.Visible;
    }

    /// <summary>Hides the reply preview bar.</summary>
    public void HideReplyPreview()
    {
        ReplyPreview.Visibility = Visibility.Collapsed;
    }

    private void CancelReply_Click(object sender, RoutedEventArgs e)
    {
        HideReplyPreview();
        ReplyCancelled?.Invoke();
    }

    private void OnTopicToggled(object sender, RoutedEventArgs e)
    {
        var expanded = TopicToggleButton.IsChecked == true;
        SubjectInputBox.Visibility = expanded ? Visibility.Visible : Visibility.Collapsed;
        if (!expanded)
        {
            // The clear is part of the collapse, which the manager records on its
            // own (`toggle_topic` → no subject). Forwarded as an edit it would store
            // an EMPTY subject — `Some("")` — which reads as expanded again.
            _lastSubjectEmitted = string.Empty;
            SubjectInputBox.Text = string.Empty;
        }
        if (expanded == _lastTopicEmitted) return;
        _lastTopicEmitted = expanded;
        TopicToggled?.Invoke(expanded);
    }

    private void SubjectInput_TextChanged(object sender, TextChangedEventArgs e)
    {
        var text = SubjectInputBox.Text;
        if (text == _lastSubjectEmitted) return;
        _lastSubjectEmitted = text;
        SubjectChanged?.Invoke(text);
    }

    private async void AttachFile_Click(object sender, RoutedEventArgs e)
    {
        var picker = new Windows.Storage.Pickers.FileOpenPicker();
        picker.FileTypeFilter.Add("*");

        // WinUI 3 requires initializing the picker with the window handle
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(App.MainWindow!);
        WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);

        var file = await picker.PickSingleFileAsync();
        if (file is null) return;

        var buffer = await Windows.Storage.FileIO.ReadBufferAsync(file);
        var bytes = new byte[buffer.Length];
        using (var reader = Windows.Storage.Streams.DataReader.FromBuffer(buffer))
        {
            reader.ReadBytes(bytes);
        }

        var mediaType = file.ContentType ?? "application/octet-stream";

        // Strip EXIF for images
        if (mediaType.StartsWith("image/", StringComparison.OrdinalIgnoreCase))
        {
            bytes = ExifStripper.Strip(bytes);
        }

        AttachmentPicked?.Invoke(file.Name, mediaType, bytes);
    }

    private async void SendButton_Click(object sender, RoutedEventArgs e)
    {
        var text = MessageText;
        if (string.IsNullOrWhiteSpace(text) && !_hasAttachments) return;

        if (SendRequested is not null)
        {
            await SendRequested.Invoke(text);
        }
    }
}
