using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Helpers;
using FaunaApp.Services;
using FaunaApp.Sync;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;
using uniffi.fauna_labeler_catalog_machine;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Personalization sub-page (content-moderation-and-ranking.md §
/// Composition + § Tier-3 community models; ui.yaml personalization). A thin
/// renderer over the shared-Rust <see cref="LabelerCatalogMachine"/>
/// (libs/fauna-labeler-catalog-machine, via libs/fauna-ffi) — renders only the
/// SUBSCRIBED-labelers slice (<see cref="LabelerCatalogPage"/> renders the full
/// catalog off its own machine instance; the windows per-page-machine
/// convention, mirrors <see cref="DevicesPage"/>/<see cref="FoldersPage"/>
/// rather than linux's per-settings-shell shared machine).
///
/// Mirrors apps/fauna-linux/src/views/personalization/mod.rs (the lift
/// reference this page tracks 1:1): "Feeds" exits Settings to the Feed page,
/// "Muted words" switches to the already-shipped muted-words sub-page, and
/// "Community labelers" (subscribed rows, ORIGINAL snapshot indices preserved
/// so Unsubscribe targets the right row) links out to the full catalog.
///
/// <para>⚠ No <c>ConfigureAwait(false)</c> — a WinUI page that mutates bound
/// state off the UI thread throws a silent <c>COMException</c>
/// (reference_windows_vm_configureawait_comexception).</para>
/// </summary>
public sealed partial class PersonalizationPage : Page
{
    private INestRpcClient? _rpc;

    private LabelerCatalogMachine? _machine;
    private LabelerCatalogNotifyObserver? _observer;

    /// <summary>Whether the last <see cref="RenderPage"/> pass rendered at least one
    /// subscribed-labeler row — tracked so a transition into non-empty (not every
    /// render) is what triggers <see cref="BringIntoViewAfterLayout"/> on
    /// <see cref="LabelersContainer"/>, avoiding a scroll-jack on every unrelated
    /// notify tick once rows already exist.</summary>
    private bool _hadSubscribedRows;

    private TrainedTopicsViewModel? _trainedTopicsVm;
    /// <summary>Non-null while the name-input is retargeted at a rename (the
    /// row's id); reset to null after any successful commit, mirroring linux's
    /// <c>Ctx.renaming</c>.</summary>
    private byte[]? _renamingTopicId;

    // ── Publish review-prune sheet (topic-factors.md § Publishing a trained
    // factor; frame D8) — state lives on the page, mirroring where
    // _renamingTopicId already lives (not the VM): the sheet's whole job is
    // building/toggling WinUI widgets, which the VM has no business holding.

    /// <summary>The factor id (16-byte registry id) the open sheet targets;
    /// <c>null</c> while closed. Mirrors linux <c>publish_sheet::Ctx.target</c>.</summary>
    private byte[]? _publishTargetFactorId;

    /// <summary>The same target's <c>topic:&lt;hex&gt;</c> composition key —
    /// what corpus scoring/scrubbing addresses the model by. Stored alongside
    /// <see cref="_publishTargetFactorId"/> (not re-derived) so a kind swap
    /// can re-read the corpus without the caller threading it through again;
    /// mirrors apple's <c>publishTargetFactorKey</c>.</summary>
    private string? _publishTargetFactorKey;

    /// <summary>Bumped on every open/close of the publish sheet. The corpus
    /// read is async (a nest round trip for the sealed model), so without this
    /// a result scored for factor A can land AFTER the sheet was re-opened
    /// against factor B (or closed) and render A's exemplars under B's target
    /// — submit would then publish A's posts under B's derived key and B's
    /// chosen name. <see cref="RenderPublishExemplars"/> is only ever invoked
    /// by a caller that already checked its captured generation against this
    /// field. Mirrors linux <c>Ctx.generation</c> / `publish_sheet.rs:79-88,
    /// 240-286,306`.</summary>
    private int _publishGeneration;

    /// <summary>The scored exemplars paired with the checkbox that decides
    /// each one's fate — submit reads this; nothing else does. Mirrors linux
    /// <c>Ctx.rows</c>.</summary>
    private readonly List<(ScoredExemplar Exemplar, CheckBox Include)> _publishRows = new();

    // ── The Model kind (topic-factors.md § Publishing a trained factor, v2) ──
    // The List section's exact shape, one axis over: a raw wire discriminator
    // picks which corpus face + review body is live, and the two kinds never
    // share state (a kind swap discards the other's prune — set_publish_kind_op's
    // reasoning, mirrored from tui/apple).

    /// <summary>The open sheet's raw wire discriminator — <c>"list"</c>
    /// (default, the weaker disclosure) or <c>"text-model"</c>. Never a
    /// translated label: the kind select round-trips this directly
    /// (<c>backup-destination-kind-select</c>'s rule).</summary>
    private string _publishKind = "list";

    /// <summary>The Model kind's survivors paired with their include checkbox
    /// — the exemplar rows' exact shape, one type over.</summary>
    private readonly List<(ReviewNgram Ngram, CheckBox Include)> _publishNgramRows = new();

    /// <summary>The Model corpus-size facts the mandated copy states —
    /// carried UNSHRUNK through to publish regardless of what the review
    /// prunes (they are the posterior's priors, not the pruned entry count).</summary>
    private uint _publishMoreDocs;
    private uint _publishLessDocs;
    private uint _publishIncludedExamples;
    private uint _publishMarkedExamples;

    private SignalShareViewModel? _signalShareVm;
    /// <summary>Set while RenderPage programmatically updates the signal-share
    /// toggle, so its Toggled handler doesn't echo the change back as a dispatch
    /// (mirrors MailSpamPanel's <c>_rsSyncing</c>).</summary>
    private bool _signalShareSyncing;

    /// <summary>Page-scoped error from a direct-writer (Page_Loaded's initial
    /// load, the publish sheet's open/submit, clear-engagement-data) — none of
    /// which flow through the trained-topics/signal-share VM or the machine's
    /// own snapshot.error. <see cref="RenderPage"/>'s precedence chain includes
    /// this BELOW the VM/machine tiers so an unrelated <see cref="RenderPage"/>
    /// call (a toggle, a rename, a catalog notify tick) can never silently
    /// reclaim-and-clear it. Set/cleared only via <see cref="ShowPageError"/> /
    /// <see cref="ClearPageError"/> — never assign <c>ErrorBar</c> directly
    /// elsewhere in this file.</summary>
    private string? _pageError;

    public PersonalizationPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _rpc = clients.Rpc;
            if (clients.Rpc is not null)
            {
                _trainedTopicsVm = new TrainedTopicsViewModel(clients.Rpc);
                _signalShareVm = new SignalShareViewModel(clients.Rpc);
            }
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        // Pure shared-Rust catalog, no nest state — built once, not
        // repopulated per sheet open (mirrors BackupsPage.PopulateDestinationKindSelect).
        PopulatePublishKindSelect();

        if (_rpc is null) return;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            // Build the page machine bound to the session WS-RPC requester; the
            // observer ticks the UI thread (LabelerCatalogNotifyObserver) → RenderPage.
            _observer = new LabelerCatalogNotifyObserver(RenderPage);
            _machine = await _rpc.BuildLabelerCatalogMachineAsync(_observer);
            await _machine.Refresh();
            if (_trainedTopicsVm is not null) await _trainedTopicsVm.LoadAsync();
            if (_signalShareVm is not null) await _signalShareVm.LoadAsync();
        }
        catch (Exception ex)
        {
            ShowPageError(Strings.Error(ex));
        }
        LoadProgress.IsActive = false;
        LoadProgress.Visibility = Visibility.Collapsed;
        RenderPage();
    }

    /// <summary>Scroll <paramref name="panel"/> into the ScrollViewer's viewport once
    /// layout has run — this page is taller than the e2e viewport, so a just-revealed
    /// element below the Feeds/Muted-words/Trained-topics/Signal-share facets is in the
    /// UIA tree but OFFSCREEN, and the driver's bare <c>is_visible</c> is
    /// <c>!IsOffscreen</c> (reference_windows_e2e_is_visible_offscreen; mirrors
    /// AtprotoPage.BringIntoViewAfterLayout). <c>UpdateLayout()</c> must run BEFORE
    /// <c>StartBringIntoView()</c> — called in the same pass as a Visibility flip it is
    /// a silent no-op, since the panel has not been measured yet.</summary>
    private void BringIntoViewAfterLayout(FrameworkElement panel)
    {
        panel.UpdateLayout();
        panel.StartBringIntoView();
        DispatcherQueue?.TryEnqueue(
            Microsoft.UI.Dispatching.DispatcherQueuePriority.Low,
            () => panel.StartBringIntoView());
    }

    private void RenderPage()
    {
        if (_machine is null) return;
        var snap = _machine.Snapshot();

        // Only the caller's SUBSCRIBED labelers, ORIGINAL indices preserved into
        // the full entries array so Unsubscribe targets the right row (mirrors
        // linux's rebuild_rows `indices` param — the machine's gestures are
        // index-addressed into the full, unfiltered snapshot).
        LabelersContainer.Children.Clear();
        var subscribed = snap.entries
            .Select((entry, index) => (Entry: entry, Index: (uint)index))
            .Where(x => x.Entry.subscribed)
            .ToList();
        foreach (var (entry, index) in subscribed)
        {
            var row = LabelerCatalogRowBuilder.BuildRow(
                entry,
                index,
                showInspectSubscribe: false,
                onInspect: _ => Task.CompletedTask,
                onSubscribe: _ => Task.CompletedTask,
                onUnsubscribe: i => _machine!.Unsubscribe(i));
            LabelersContainer.Children.Add(row);
        }
        // Below-the-fold on this long page, same as LabelersEmptyText below: bring the
        // list into view on the empty→non-empty transition (reference_windows_e2e_is_
        // visible_offscreen) so a bare is_visible on a subscribed row's unsubscribe
        // button doesn't read offscreen right after subscribing.
        if (subscribed.Count > 0 && !_hadSubscribedRows)
        {
            BringIntoViewAfterLayout(LabelersContainer);
        }
        _hadSubscribedRows = subscribed.Count > 0;

        // Loaded AND empty — gating on `loaded` stops the empty state from painting
        // over a read still in flight (docs/goal/ui/README.md § List pages: loading
        // is not empty).
        var wasEmptyTextVisible = LabelersEmptyText.Visibility == Visibility.Visible;
        LabelersEmptyText.Visibility = snap.loaded && subscribed.Count == 0
            ? Visibility.Visible
            : Visibility.Collapsed;
        if (LabelersEmptyText.Visibility == Visibility.Visible && !wasEmptyTextVisible)
        {
            BringIntoViewAfterLayout(LabelersEmptyText);
        }

        RenderTrainedTopics();
        RenderSignalShare();

        // Page-level error-message: the machine localizes the last read/gesture
        // failure into snapshot.error; a subsequent successful gesture clears it.
        // The Trained-topics VM's own ErrorMessage wins when set (a trained-topics
        // gesture is usually the more recent user action), then the signal-share
        // VM's, then the machine's own, then finally _pageError — a direct-writer's
        // error (Page_Loaded's initial load, the publish sheet's open/submit,
        // clear-engagement-data) set via ShowPageError below. _pageError sits
        // BELOW the VM/machine tiers but ABOVE the final "else" clear so an
        // unrelated RenderPage() call (a toggle, a rename, a catalog notify tick)
        // can never silently reclaim-and-clear a still-relevant direct-written
        // error — only an explicit ClearPageError() call retires it: at the
        // START of a fresh direct-writer operation, or on that SAME operation's
        // success (see ClearPageError's own doc for which call sites use which),
        // mirroring TrainedTopicsViewModel.RunAsync's own ErrorMessage lifecycle.
        // RenderPage itself never calls ClearPageError.
        var err = snap.error;
        var trainedErr = _trainedTopicsVm?.ErrorMessage;
        var signalErr = _signalShareVm?.ErrorMessage;
        if (!string.IsNullOrEmpty(trainedErr))
        {
            ErrorBar.Message = trainedErr;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = trainedErr;
        }
        else if (!string.IsNullOrEmpty(signalErr))
        {
            ErrorBar.Message = signalErr;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = signalErr;
        }
        else if (err is not null)
        {
            var msg = S.Resolve(err);
            ErrorBar.Message = msg;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
        }
        else if (!string.IsNullOrEmpty(_pageError))
        {
            ErrorBar.Message = _pageError;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = _pageError;
        }
        else
        {
            ErrorBar.IsOpen = false;
            App.CurrentErrorMessage = null;
        }
    }

    /// <summary>Set a page-scoped error from a direct-writer that doesn't flow
    /// through the trained-topics/signal-share VM or machine snapshot (Page_Loaded's
    /// initial load, the publish sheet's open/submit, clear-engagement-data).
    /// Writes <see cref="ErrorBar"/> immediately — most call sites here aren't
    /// followed by a <see cref="RenderPage"/> call — AND records
    /// <see cref="_pageError"/> so a LATER unrelated <see cref="RenderPage"/> call
    /// includes it in the precedence chain instead of clearing it out from under
    /// this write (the bug this pair of helpers fixes).</summary>
    private void ShowPageError(string message)
    {
        _pageError = message;
        ErrorBar.Message = message;
        ErrorBar.IsOpen = true;
        App.CurrentErrorMessage = message;
    }

    /// <summary>Retire a page-scoped error set by <see cref="ShowPageError"/> —
    /// call either (a) at the point the SAME operation that raised it succeeds
    /// (<see cref="SubmitPublishAsync"/>, <see cref="DeleteCueRollupAsync"/>),
    /// or (b) at the very START of a fresh, discrete user-initiated operation
    /// (<see cref="OpenPublishSheetAsync"/>), mirroring
    /// <c>TrainedTopicsViewModel.RunAsync</c>'s own <c>ErrorMessage</c>, which
    /// clears at the start of every attempt, not just on success. (b) may
    /// retire an error a DIFFERENT direct-writer raised — that's intentional:
    /// the user has moved on to a new flow, and that flow's own
    /// <see cref="ShowPageError"/> re-surfaces anything that goes wrong with
    /// THIS attempt. What must never clear it is an incidental, non-user-
    /// initiated call like <see cref="RenderPage"/> (a toggle, a rename, a
    /// catalog notify tick) — that's the bug this pair of helpers fixes.</summary>
    private void ClearPageError()
    {
        _pageError = null;
        ErrorBar.IsOpen = false;
        App.CurrentErrorMessage = null;
    }

    /// <summary>Reflect the signal-share VM's state into the toggle + published
    /// list (engagement-cues.md § Layer B). The toggle's realization/render echo
    /// is guarded by <see cref="_signalShareSyncing"/> so this doesn't dispatch a
    /// redundant set. Mirrors <c>MailSpamPanel.RenderState</c>'s report-share
    /// section.</summary>
    private void RenderSignalShare()
    {
        if (_signalShareVm is null) return;

        if (ShareSignalsToggle.IsOn != _signalShareVm.ShareSignals)
        {
            _signalShareSyncing = true;
            ShareSignalsToggle.IsOn = _signalShareVm.ShareSignals;
            _signalShareSyncing = false;
        }
        // Carry the toggle's on/off for the e2e driver via the `state` attr (the
        // uniform get_attr(id, "state") idiom, mirroring mail-spam's
        // report-share toggle) — non-optimistic, set only from the VM's
        // nest-confirmed value, so it doubles as the round-trip proof.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            ShareSignalsToggle, _signalShareVm.ShareSignals ? "on" : "off");

        SignalPublishedList.ItemsSource = _signalShareVm.PublishedSignals;
        SignalPublishedEmpty.Visibility =
            _signalShareVm.PublishedSignals.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>Rebuild the <c>personalization-trained-factor-item</c> rows from
    /// the VM's current list.</summary>
    private void RenderTrainedTopics()
    {
        if (_trainedTopicsVm is null) return;
        TrainedTopicsContainer.Children.Clear();
        foreach (var row in _trainedTopicsVm.Topics)
            TrainedTopicsContainer.Children.Add(BuildTrainedTopicRow(row));
        TrainedTopicsEmptyText.Visibility =
            _trainedTopicsVm.Topics.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>One <c>personalization-trained-factor-item</c> row: name +
    /// example count + rename/delete. The row's own <c>AutomationProperties.Name</c>
    /// keeps it un-pruned (a bare-id-only container is invisible to FlaUI); its
    /// <c>HelpText</c> carries the factor id's hex (<c>get_attr(item, "factor")</c>
    /// — the sealed registry gives the e2e no wire-side way to learn the minted
    /// key it must pass to <c>feed-factor-select</c>'s <c>select()</c>).</summary>
    private FrameworkElement BuildTrainedTopicRow(FfiTrainedTopicRow topic)
    {
        var row = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 12,
            Padding = new Thickness(8),
        };
        var idHex = Convert.ToHexString(topic.id).ToLowerInvariant();
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.PersonalizationTrainedFactorItem);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, topic.name);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(row, idHex);

        var name = new TextBlock { Text = topic.name, VerticalAlignment = VerticalAlignment.Center };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(name, Ids.PersonalizationTrainedFactorName);
        row.Children.Add(name);

        var count = new TextBlock
        {
            Text = S.Format("personalization/trained_factor_examples", topic.exampleCount.ToString()),
            Opacity = 0.6,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(count, Ids.PersonalizationTrainedFactorExampleCount);
        row.Children.Add(count);

        // The Layer-A opt-in (engagement-cues.md § Layer A): watch/skip engagement
        // cues weak-train this factor only while this is on. IsOn is set BEFORE
        // attaching Toggled so the initial render never fires an echo
        // (reference_windows_flaui_state_attr_helptext; mirrors FoldersPage's
        // folder-webdav-toggle). HelpText carries the literal "true"/"false" the
        // cross-app engagement_toggle_on() reads via get_attr(id, "state")
        // (personalization.py) — NOT the "on"/"off" idiom the nest-confirmed
        // report-share/serve-here toggles use, since this is the bare-Switch
        // idiom linux's gtk::Switch answers natively.
        var engagementToggle = new ToggleSwitch
        {
            Header = S.Get("personalization/trained_factor_engagement_toggle"),
            Tag = topic.id,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(engagementToggle, Ids.PersonalizationTrainedFactorEngagementToggle);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(engagementToggle, topic.learnFromEngagement ? "true" : "false");
        engagementToggle.IsOn = topic.learnFromEngagement;
        engagementToggle.Toggled += TrainedTopicEngagementToggle_Toggled;
        row.Children.Add(engagementToggle);

        var renameBtn = new Button
        {
            Content = S.Get("personalization/trained_factor_rename"),
            Tag = topic,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(renameBtn, Ids.PersonalizationTrainedFactorRenameButton);
        renameBtn.Click += TrainedTopicRenameButton_Click;
        row.Children.Add(renameBtn);

        // Publish… — open the review-prune sheet against THIS factor
        // (topic-factors.md § Publishing a trained factor; frame D8). Opening
        // only reveals a sheet: nothing leaves the device until the user
        // prunes, names the list, and submits. A corrupt (non-16-byte) id has
        // no addressable model — nothing to score, nothing to publish — so
        // the row simply offers no publish (mirrors linux's
        // `publish_btn.set_sensitive(key.is_some())`).
        var publishBtn = new Button
        {
            Content = S.Get("personalization/trained_factor_publish"),
            Tag = topic,
            VerticalAlignment = VerticalAlignment.Center,
            IsEnabled = topic.factorKey is not null,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(publishBtn, Ids.PersonalizationTrainedFactorPublishButton);
        publishBtn.Click += TrainedTopicPublishButton_Click;
        row.Children.Add(publishBtn);

        var deleteBtn = new Button
        {
            Content = S.Get("common/delete"),
            Tag = topic.id,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(deleteBtn, Ids.PersonalizationTrainedFactorDeleteButton);
        deleteBtn.Click += TrainedTopicDeleteButton_Click;
        row.Children.Add(deleteBtn);

        return row;
    }

    private async void TrainedTopicCreateButton_Click(object sender, RoutedEventArgs e) => await SubmitTrainedTopicAsync();

    private async void TrainedTopicNameInput_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Enter) await SubmitTrainedTopicAsync();
    }

    /// <summary>Commit the name-input: a rename when a row retargeted it
    /// (<see cref="_renamingTopicId"/>), else a create. Any successful commit
    /// resets the input to create mode.</summary>
    private async Task SubmitTrainedTopicAsync()
    {
        if (_trainedTopicsVm is null) return;
        var name = TrainedTopicNameInput.Text;
        if (string.IsNullOrWhiteSpace(name)) return;

        var ok = _renamingTopicId is { } id
            ? await _trainedTopicsVm.RenameAsync(id, name)
            : await _trainedTopicsVm.CreateAsync(name);

        if (ok)
        {
            _renamingTopicId = null;
            TrainedTopicNameInput.Text = string.Empty;
            TrainedTopicCreateButton.Content = S.Get("personalization/trained_factor_create");
        }
        RenderPage();
    }

    private void TrainedTopicRenameButton_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: FfiTrainedTopicRow topic }) return;
        _renamingTopicId = topic.id;
        TrainedTopicNameInput.Text = topic.name;
        TrainedTopicNameInput.Focus(FocusState.Programmatic);
        TrainedTopicCreateButton.Content = S.Get("personalization/trained_factor_save");
    }

    private async void TrainedTopicDeleteButton_Click(object sender, RoutedEventArgs e)
    {
        if (_trainedTopicsVm is null || sender is not Button { Tag: byte[] id }) return;
        await _trainedTopicsVm.DeleteAsync(id);
        RenderPage();
    }

    /// <summary>personalization-trained-factor-engagement-toggle: flip the row's
    /// Layer-A opt-in. Always re-renders (success reflects the nest-confirmed
    /// value; a failure leaves <see cref="_trainedTopicsVm"/>'s row list — and so
    /// the rebuilt toggle's initial state — unchanged, snapping it back).</summary>
    private async void TrainedTopicEngagementToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_trainedTopicsVm is null || sender is not ToggleSwitch { Tag: byte[] id } toggle) return;
        await _trainedTopicsVm.SetLearnFromEngagementAsync(id, toggle.IsOn);
        RenderPage();
    }

    // ── Publish review-prune sheet (topic-factors.md § Publishing a trained
    // factor; frame D8) — ports apps/fauna-linux/src/views/personalization/
    // publish_sheet.rs. Single-instance, pre-targeted (the admin-dns-rename-
    // sheet shape): built once in PersonalizationPage.xaml, Collapsed until a
    // row's publish button reveals it against that row's factor.

    private async void TrainedTopicPublishButton_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: FfiTrainedTopicRow topic } || topic.factorKey is not { } factorKey) return;
        await OpenPublishSheetAsync(topic.id, factorKey);
    }

    /// <summary>Reveal the sheet against one factor and kick off its async
    /// corpus score. <paramref name="factorId"/> is the registry's 16-byte id
    /// (what publishing derives the signing key from); <paramref name="factorKey"/>
    /// is the same id's <c>topic:&lt;hex&gt;</c> composition key (what scoring
    /// addresses the model by) — both come off the row, never re-derived
    /// here.
    ///
    /// Retargeting + generation guard: bumping <see cref="_publishGeneration"/>
    /// here and capturing it BEFORE the corpus-score await means a read still
    /// in flight for a PREVIOUS open (or one issued before this sheet was
    /// closed/re-opened against a different factor) is dropped on completion
    /// rather than rendered under the wrong target — mirrors linux
    /// <c>PublishSheet::open</c>.
    ///
    /// <see cref="ClearPageError"/> at the very top mirrors
    /// <c>TrainedTopicsViewModel.RunAsync</c> clearing its own <c>ErrorMessage</c>
    /// at the START of every attempt (not just on success): a fresh sheet-open
    /// is itself a natural reset point, so it retires a stale error from a
    /// PRIOR failed publish attempt whether this is a same-factor retry or a
    /// fresh open against a DIFFERENT factor. Otherwise nothing ever clears it
    /// on the success path here (<see cref="RenderPublishExemplars"/> doesn't),
    /// so a transient corpus-score hiccup left the banner stuck showing a
    /// resolved problem indefinitely (2026-07-19 re-review).</summary>
    private async Task OpenPublishSheetAsync(byte[] factorId, string factorKey)
    {
        ClearPageError();
        _publishTargetFactorId = factorId;
        _publishTargetFactorKey = factorKey;
        var generation = ++_publishGeneration;
        _publishRows.Clear();
        PublishExemplarContainer.Children.Clear();
        _publishNgramRows.Clear();
        PublishNgramContainer.Children.Clear();
        _publishMoreDocs = _publishLessDocs = _publishIncludedExamples = _publishMarkedExamples = 0;
        // Starts BLANK on every open — never prefilled from the sealed
        // registry name (topic-factors.md § Publishing).
        PublishNameInput.Text = string.Empty;
        PublishExemplarEmpty.Visibility = Visibility.Collapsed;
        PublishNgramEmpty.Visibility = Visibility.Collapsed;
        // Nothing is reviewable until the corpus read lands.
        PublishSubmitButton.IsEnabled = false;
        PublishSheet.Visibility = Visibility.Visible;
        PublishNameInput.Focus(FocusState.Programmatic);

        // Always reopens on the WEAKER disclosure (topic-factors.md §
        // Publishing) — a redundant re-select of the already-current "list"
        // is a no-op in SetPublishKindAsync's own guard, so this also covers
        // "the sheet was already on Model and closed without swapping back".
        _publishKind = "list";
        SelectPublishKind("list");
        UpdatePublishLimitationNote();
        PublishExemplarSection.Visibility = Visibility.Visible;
        PublishNgramSection.Visibility = Visibility.Collapsed;

        if (_trainedTopicsVm is null) return;
        try
        {
            var exemplars = await _trainedTopicsVm.ScoreCorpusForFactorAsync(factorKey);
            // A close or re-open since the await means these exemplars belong
            // to another factor (or nobody current) — rendering them would let
            // the user publish them under the CURRENT target's identity.
            if (generation != _publishGeneration) return;
            RenderPublishExemplars(exemplars);
        }
        catch (Exception ex)
        {
            if (generation != _publishGeneration) return;
            ShowPageError(Strings.Error(ex));
        }
    }

    /// <summary>Rebuild the <c>personalization-trained-factor-publish-exemplar-item</c>
    /// rows from a corpus-score result already verified fresh by the caller
    /// (the generation check happens in <see cref="OpenPublishSheetAsync"/>,
    /// not here — mirrors linux's <c>render_exemplars</c>, called only from
    /// behind its own generation check).</summary>
    private void RenderPublishExemplars(ScoredExemplar[] exemplars)
    {
        PublishExemplarContainer.Children.Clear();
        _publishRows.Clear();
        foreach (var exemplar in exemplars)
        {
            var (row, include) = BuildPublishExemplarRow(exemplar);
            PublishExemplarContainer.Children.Add(row);
            _publishRows.Add((exemplar, include));
        }
        PublishExemplarEmpty.Visibility = exemplars.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
        RefreshPublishSubmitSensitivity();
    }

    /// <summary>One scored exemplar row: preview text + per-mille score + an
    /// include checkbox, default-CHECKED (the <c>restore-kind-checkbox</c>
    /// prune shape — the user edits the factor's own proposal rather than
    /// assembling one). The checkbox's Checked/Unchecked handlers attach
    /// AFTER the initial <c>IsChecked = true</c>, so building a row never
    /// echoes a toggle into the submit-sensitivity read
    /// (reference_windows_flaui_state_attr_helptext).</summary>
    private (FrameworkElement Row, CheckBox Include) BuildPublishExemplarRow(ScoredExemplar exemplar)
    {
        var row = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            Padding = new Thickness(4),
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.PersonalizationTrainedFactorPublishExemplarItem);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, exemplar.preview);

        var include = new CheckBox { IsChecked = true, VerticalAlignment = VerticalAlignment.Center };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(include, Ids.PersonalizationTrainedFactorPublishExemplarCheckbox);
        // get_attr(id, "state") reads AutomationProperties.HelpText (the bare-
        // Switch/CheckBox idiom the engagement toggle already uses) — literal
        // "true"/"false", not the nest-confirmed "on"/"off" idiom.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(include, "true");
        include.Checked += PublishExemplarCheckbox_Toggled;
        include.Unchecked += PublishExemplarCheckbox_Toggled;
        row.Children.Add(include);

        var text = new TextBlock
        {
            Text = exemplar.preview,
            TextWrapping = TextWrapping.NoWrap,
            TextTrimming = TextTrimming.CharacterEllipsis,
            MaxWidth = 320,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(text, Ids.PersonalizationTrainedFactorPublishExemplarText);
        row.Children.Add(text);

        // The per-mille the artifact will carry verbatim — the same number a
        // subscriber reads at inspect, with no rescale between here and there.
        var score = new TextBlock
        {
            Text = S.Format("personalization/publish_score", exemplar.score.ToString()),
            Opacity = 0.6,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(score, Ids.PersonalizationTrainedFactorPublishExemplarScore);
        row.Children.Add(score);

        return (row, include);
    }

    /// <summary>Publishing nothing is not a thing the artifact means, so the
    /// button says so by going insensitive rather than no-op'ing a click (the
    /// <c>restore-confirm-button</c> precedent). Covers both "the corpus
    /// scored nothing" and "the user unchecked everything" — for whichever
    /// kind is currently open (the other kind's rows are stale/empty by
    /// construction, never both live at once).</summary>
    private void RefreshPublishSubmitSensitivity()
    {
        PublishSubmitButton.IsEnabled = _publishKind == "text-model"
            ? _publishNgramRows.Any(r => r.Include.IsChecked == true)
            : _publishRows.Any(r => r.Include.IsChecked == true);
    }

    private void PublishExemplarCheckbox_Toggled(object sender, RoutedEventArgs e)
    {
        if (sender is not CheckBox cb) return;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(cb, cb.IsChecked == true ? "true" : "false");
        RefreshPublishSubmitSensitivity();
    }

    /// <summary>Build <c>personalization-trained-factor-publish-kind-select</c>'s
    /// items from the shared catalog (<see cref="ValueFormat.PublishKindOptions"/>)
    /// — Content = the localized label, Tag + AutomationProperties.Name = the
    /// WIRE value. Name carries the wire value (never the label) because the
    /// shared e2e's <c>driver.select</c>/<c>get_text</c> match a ComboBoxItem
    /// by Name EXACTLY on windows (the <c>backup-destination-kind-select</c>
    /// precedent). Pure catalog, no state — built once, not repopulated per
    /// sheet open.</summary>
    private void PopulatePublishKindSelect()
    {
        PublishKindSelect.Items.Clear();
        foreach (var (value, label) in ValueFormat.PublishKindOptions())
        {
            var item = new ComboBoxItem { Content = label, Tag = value };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, value);
            PublishKindSelect.Items.Add(item);
        }
    }

    /// <summary>Preselect <paramref name="wire"/> in the kind select without
    /// inventing a selection for a value not among the painted options.</summary>
    private void SelectPublishKind(string wire)
    {
        foreach (var obj in PublishKindSelect.Items)
        {
            if (obj is ComboBoxItem { Tag: string tag } item && tag == wire)
            {
                PublishKindSelect.SelectedItem = item;
                return;
            }
        }
    }

    /// <summary><c>personalization-trained-factor-publish-kind-select</c>:
    /// swap which artifact kind the open sheet reviews.</summary>
    private async void PublishKindSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (PublishKindSelect.SelectedItem is not ComboBoxItem { Tag: string kind }) return;
        await SetPublishKindAsync(kind);
    }

    /// <summary>Swap which artifact kind the open sheet reviews. Re-reads from
    /// scratch — the two kinds review different objects through different
    /// shared faces, so carrying either kind's state across the swap would
    /// paint one kind's refusal/prune over the other's un-read corpus (mirrors
    /// tui <c>set_publish_kind_op</c> / apple <c>setPublishKind</c>). Picking
    /// the kind already selected is a no-op — a redundant select must not
    /// discard a review in progress. The public name survives the swap (not
    /// touched here).</summary>
    private async Task SetPublishKindAsync(string kind)
    {
        if (kind == _publishKind || _publishTargetFactorId is null
            || _publishTargetFactorKey is not { } factorKey) return;
        _publishKind = kind;
        _publishRows.Clear();
        PublishExemplarContainer.Children.Clear();
        PublishExemplarEmpty.Visibility = Visibility.Collapsed;
        _publishNgramRows.Clear();
        PublishNgramContainer.Children.Clear();
        PublishNgramEmpty.Visibility = Visibility.Collapsed;
        _publishMoreDocs = _publishLessDocs = _publishIncludedExamples = _publishMarkedExamples = 0;
        PublishSubmitButton.IsEnabled = false;

        var isModel = kind == "text-model";
        PublishExemplarSection.Visibility = isModel ? Visibility.Collapsed : Visibility.Visible;
        PublishNgramSection.Visibility = isModel ? Visibility.Visible : Visibility.Collapsed;
        UpdatePublishLimitationNote();

        if (_trainedTopicsVm is null) return;
        var generation = ++_publishGeneration;
        try
        {
            if (isModel)
            {
                var review = await _trainedTopicsVm.ScrubCorpusForFactorAsync(factorKey);
                if (generation != _publishGeneration) return;
                RenderPublishNgrams(review);
            }
            else
            {
                var exemplars = await _trainedTopicsVm.ScoreCorpusForFactorAsync(factorKey);
                if (generation != _publishGeneration) return;
                RenderPublishExemplars(exemplars);
            }
        }
        catch (Exception ex)
        {
            if (generation != _publishGeneration) return;
            ShowPageError(Strings.Error(ex));
        }
    }

    /// <summary>The mandated per-kind copy (topic-factors.md § Publishing —
    /// absence of any one disclosure is a bug): List states its corpus limit +
    /// anonymity; Model states generalization + the ≥3-doc pattern disclosure
    /// (incl. the dislike half) + anonymity/explicit-only, plus the corpus-size
    /// line the rebuild's drops must stay visible through (mirrors apple's
    /// concatenation exactly).</summary>
    private void UpdatePublishLimitationNote()
    {
        PublishLimitationNote.Text = _publishKind == "text-model"
            ? S.Get("personalization/publish_limitation_note_model") + "\n" +
              S.Format("personalization/publish_corpus_size",
                  _publishIncludedExamples.ToString(), _publishMarkedExamples.ToString())
            : S.Get("personalization/publish_limitation_note");
    }

    /// <summary>Rebuild the <c>personalization-trained-factor-publish-ngram-item</c>
    /// rows from a corpus-scrub result already verified fresh by the caller
    /// (the generation check happens in <see cref="SetPublishKindAsync"/>, not
    /// here — mirrors <see cref="RenderPublishExemplars"/>'s own shape).</summary>
    private void RenderPublishNgrams(TrainedModelReview review)
    {
        _publishMoreDocs = review.moreDocs;
        _publishLessDocs = review.lessDocs;
        _publishIncludedExamples = review.includedExamples;
        _publishMarkedExamples = review.markedExamples;
        UpdatePublishLimitationNote();

        PublishNgramContainer.Children.Clear();
        _publishNgramRows.Clear();
        foreach (var ngram in review.ngrams)
        {
            var (row, include) = BuildPublishNgramRow(ngram);
            PublishNgramContainer.Children.Add(row);
            _publishNgramRows.Add((ngram, include));
        }
        PublishNgramEmpty.Visibility = review.ngrams.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
        RefreshPublishSubmitSensitivity();
    }

    /// <summary>One surviving n-gram row: text + class direction + distinct-
    /// document count (BOTH shared faces — <see cref="ValueFormat.NgramDirectionLabel"/>/
    /// <see cref="ValueFormat.NgramDocCountLabel"/> — so a publisher's review
    /// and a subscriber's <c>labeler-inspect-model-entry-*</c> read cannot
    /// disagree about what the counts mean) + an include checkbox, default-
    /// CHECKED. <see cref="BuildPublishExemplarRow"/>'s exact shape, one type
    /// over.</summary>
    private (FrameworkElement Row, CheckBox Include) BuildPublishNgramRow(ReviewNgram ngram)
    {
        var row = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            Padding = new Thickness(4),
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.PersonalizationTrainedFactorPublishNgramItem);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, ngram.ngram);

        var include = new CheckBox { IsChecked = true, VerticalAlignment = VerticalAlignment.Center };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(include, Ids.PersonalizationTrainedFactorPublishNgramCheckbox);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(include, "true");
        include.Checked += PublishNgramCheckbox_Toggled;
        include.Unchecked += PublishNgramCheckbox_Toggled;
        row.Children.Add(include);

        var text = new TextBlock
        {
            Text = ngram.ngram,
            TextWrapping = TextWrapping.NoWrap,
            TextTrimming = TextTrimming.CharacterEllipsis,
            MaxWidth = 240,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(text, Ids.PersonalizationTrainedFactorPublishNgramText);
        row.Children.Add(text);

        var direction = new TextBlock
        {
            Text = ValueFormat.NgramDirectionLabel(ngram.more, ngram.less),
            Opacity = 0.6,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(direction, Ids.PersonalizationTrainedFactorPublishNgramDirection);
        row.Children.Add(direction);

        var count = new TextBlock
        {
            Text = ValueFormat.NgramDocCountLabel(ngram.more, ngram.less),
            Opacity = 0.6,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(count, Ids.PersonalizationTrainedFactorPublishNgramCount);
        row.Children.Add(count);

        return (row, include);
    }

    private void PublishNgramCheckbox_Toggled(object sender, RoutedEventArgs e)
    {
        if (sender is not CheckBox cb) return;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(cb, cb.IsChecked == true ? "true" : "false");
        RefreshPublishSubmitSensitivity();
    }

    private async void PublishSubmitButton_Click(object sender, RoutedEventArgs e) => await SubmitPublishAsync();

    /// <summary>Publish what survived the prune. Validate the name → collect
    /// checked rows → publish; on success close the sheet (drop target/rows/
    /// generation); on failure re-enable Submit, surface the localized error,
    /// and leave the user's checkbox state + name intact — mirrors linux
    /// <c>publish_sheet::submit</c>/<c>apply</c>: never clear or reset
    /// checkboxes on a failed submit.</summary>
    private async Task SubmitPublishAsync()
    {
        if (_trainedTopicsVm is null || _publishTargetFactorId is not { } factorId) return;

        // A blank name is refused here rather than at the wire — the same
        // guard the create/rename flow uses (no error dialog, just focus +
        // no-op, mirrors linux submit()'s name.is_empty() early return).
        var name = PublishNameInput.Text.Trim();
        if (name.Length == 0)
        {
            PublishNameInput.Focus(FocusState.Programmatic);
            return;
        }

        if (_publishKind == "text-model")
        {
            var ngrams = _publishNgramRows
                .Where(r => r.Include.IsChecked == true)
                .Select(r => new FfiPublishNgram(r.Ngram.ngram, r.Ngram.more, r.Ngram.less))
                .ToList();
            if (ngrams.Count == 0) return; // unreachable through the button; kept as the same invariant guard at the call

            PublishSubmitButton.IsEnabled = false;
            try
            {
                // UNSHRUNK — the corpus's own counters, not the pruned entry
                // count (they are the posterior's priors; shrinking them would
                // make the published model look more confident than it is).
                await _trainedTopicsVm.TrainedTopicPublishModelAsync(
                    factorId, name, _publishMoreDocs, _publishLessDocs, ngrams);
                ClearPageError();
                ClosePublishSheet();
            }
            catch (FfiPublishModelException ex)
            {
                PublishSubmitButton.IsEnabled = true;
                ShowPageError(TrainedTopicsViewModel.LocalizePublishModel(ex));
            }
            catch (Exception ex)
            {
                PublishSubmitButton.IsEnabled = true;
                ShowPageError(Strings.Error(ex));
            }
            return;
        }

        var entries = _publishRows
            .Where(r => r.Include.IsChecked == true)
            .Select(r => new FfiPublishEntry(r.Exemplar.postId, r.Exemplar.score))
            .ToList();
        if (entries.Count == 0) return; // unreachable through the button; kept as the same invariant guard at the call

        PublishSubmitButton.IsEnabled = false;
        try
        {
            await _trainedTopicsVm.TrainedTopicPublishListAsync(factorId, name, entries);
            ClearPageError();
            ClosePublishSheet();
        }
        catch (FfiPublishListException ex)
        {
            PublishSubmitButton.IsEnabled = true;
            ShowPageError(TrainedTopicsViewModel.LocalizePublish(ex));
        }
        catch (Exception ex)
        {
            PublishSubmitButton.IsEnabled = true;
            ShowPageError(Strings.Error(ex));
        }
    }

    private void PublishCancelButton_Click(object sender, RoutedEventArgs e) => ClosePublishSheet();

    /// <summary>Close without publishing: drop the target + bump the
    /// generation (so a stale in-flight corpus read can never render) + clear
    /// the reviewed rows, so a re-open cannot inherit a stale prune. Mirrors
    /// linux <c>close()</c>.</summary>
    private void ClosePublishSheet()
    {
        _publishTargetFactorId = null;
        _publishTargetFactorKey = null;
        _publishGeneration++;
        _publishRows.Clear();
        PublishExemplarContainer.Children.Clear();
        _publishKind = "list";
        _publishNgramRows.Clear();
        PublishNgramContainer.Children.Clear();
        _publishMoreDocs = _publishLessDocs = _publishIncludedExamples = _publishMarkedExamples = 0;
        PublishSheet.Visibility = Visibility.Collapsed;
    }

    // personalization-feeds-link: exits Settings to the top-level Feed page
    // (mirrors linux's on_navigate_to_feed — stack.set_visible_child_name("feed")
    // on the OUTER content stack, not the settings sub-stack).
    private void FeedsLinkButton_Click(object sender, RoutedEventArgs e)
        => MainPage.Current?.NavigateToView("feed");

    // personalization-muted-words-link: switches this SAME settings shell to the
    // already-shipped muted-words sub-page (reused, not rebuilt).
    private void MutedWordsLinkButton_Click(object sender, RoutedEventArgs e)
        => MainPage.Current?.NavigateToSettingsSubPage(SettingsNavigation.MutedWords);

    // personalization-browse-catalog-button: switches this SAME settings shell to
    // the full Community-labelers catalog sub-page.
    private void BrowseCatalogButton_Click(object sender, RoutedEventArgs e)
        => MainPage.Current?.NavigateToSettingsSubPage(SettingsNavigation.LabelerCatalog);

    // personalization-clear-engagement-data-button: "Clear activity data"
    // (engagement-cues.md § At rest) — user-revocable destruction of the sealed
    // cues:v1 rollup. Reached via the SAME live FfiFeedManager instance the Feed
    // page renders from (FeedViewModel.Current?.Manager — mirrors apple's
    // "deleteCueRollup() resets the SAME live engine the Feed page observes"),
    // never a second manager instance (mirrors linux's
    // crate::feed::host::manager() accessor + this page's own labeler-catalog
    // machine convention: one shared instance, not a per-page rebuild).
    private async void ClearEngagementDataButton_Click(object sender, RoutedEventArgs e)
        => await DeleteCueRollupAsync();

    /// <summary>FeedViewModel.Current is populated once the Feed page's own
    /// OnNavigatedTo constructs its view-model — which happens for every shipped
    /// navigation path today (MainPage.Page_Loaded defaults the NavigationView
    /// selection to the Feed tab, the first menu item, whenever nothing else
    /// claimed selection first). It is still nullable in principle — a
    /// state-protocol deep link straight to Settings could in theory claim
    /// selection before Feed ever loads (MainPage.xaml.cs's own comment on its
    /// "don't clobber a deep-link nav" guard) — so a null manager surfaces the
    /// page's normal not-connected error rather than throwing.</summary>
    private async Task DeleteCueRollupAsync()
    {
        var manager = FeedViewModel.Current?.Manager;
        if (manager is null)
        {
            ShowPageError(S.Get("common/not_connected"));
            return;
        }
        try
        {
            await manager.DeleteCueRollup();
            ClearPageError();
        }
        catch (Exception ex)
        {
            ShowPageError(Strings.Error(ex));
        }
    }

    // personalization-share-signals-toggle: flip the Layer-B opt-in
    // (engagement-cues.md § Layer B). Ignored while _signalShareSyncing (the
    // programmatic render echo-guard, mirrors MailSpamPanel's
    // ShareReportsToggle_Toggled).
    private async void ShareSignalsToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_signalShareSyncing || _signalShareVm is null || sender is not ToggleSwitch sw) return;
        await _signalShareVm.SetShareSignalsAsync(sw.IsOn);
        RenderPage();
    }
}
