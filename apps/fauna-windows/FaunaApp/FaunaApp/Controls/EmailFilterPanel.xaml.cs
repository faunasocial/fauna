using System.Collections.ObjectModel;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using S =FaunaApp.Core.Services.Strings;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using uniffi.fauna_ffi;
using FaunaApp.UiIds;

namespace FaunaApp.Controls;

/// <summary>
/// Represents a single email filter entry in the list.
/// </summary>
internal sealed class FilterItem
{
    public long Id { get; set; }
    public string Name { get; set; } = string.Empty;
    public string Action { get; set; } = string.Empty;

    /// <summary>The row's stored rules/action (as loaded), kept ONLY to compute
    /// <see cref="EditVisibility"/> without a per-row round-trip. The edit form
    /// itself always re-fetches fresh via <c>EmailFiltersGetAsync</c> — this is
    /// never read to populate it.</summary>
    public IReadOnlyList<FfiEmailFilterRule> Rules { get; set; } = Array.Empty<FfiEmailFilterRule>();
    public FfiEmailFilterAction ActionValue { get; set; } = new FfiEmailFilterAction.Allow();

    /// <summary>Whether <c>filter-edit</c> shows for this row — bound directly
    /// (no <c>IValueConverter</c>; the property IS a <see cref="Visibility"/>, so
    /// the classic <c>{Binding}</c> engine sets the DP with no conversion step,
    /// and this codebase has no existing bool→Visibility converter to reuse).
    /// True only when <see cref="FaunaFfiMethods.EmailFilterIsEditableFor"/> says the
    /// single-dialog-covered shape holds (single rule + a dialog-covered rule/action).</summary>
    public Visibility EditVisibility { get; set; } = Visibility.Collapsed;

    /// <summary>Whether the post-succession review pair
    /// (<c>filter-unattested-mark</c> + <c>filter-review-keep-button</c>) shows —
    /// Visible only for a rule <see cref="InheritedFilterMarks"/> holds open.</summary>
    public Visibility MarkVisibility { get; set; } = Visibility.Collapsed;
}

/// <summary>
/// UserControl for managing email filters via the nest's <c>fauna.email.*</c>
/// WS-RPC kinds (the typed <see cref="INestRpcClient"/> seam — the HTTP twins
/// were deleted nest-side).
/// </summary>
public sealed partial class EmailFilterPanel : UserControl
{
    public static readonly DependencyProperty NestClientProperty =
        DependencyProperty.Register(nameof(NestClient), typeof(INestRpcClient),
            typeof(EmailFilterPanel), new PropertyMetadata(null));

    internal INestRpcClient? NestClient
    {
        get => (INestRpcClient?)GetValue(NestClientProperty);
        set => SetValue(NestClientProperty, value);
    }

    private readonly ObservableCollection<FilterItem> _filters = new();

    /// <summary>The action kinds the form collects inputs for (<c>SUPPORTED_ACTION_KINDS</c>)
    /// — what <see cref="FaunaFfiMethods.EmailFilterIsEditableFor"/> gates a stored filter
    /// against (<c>fauna-ffi</c> exports no constant; android's list is the same).</summary>
    private static readonly string[] SupportedActionKinds = { "Allow", "Discard", "Reject", "Forward" };

    /// <summary>The id of the filter currently being edited, or <c>null</c> when
    /// <see cref="AddFilterForm"/> is in create mode. Gates whether
    /// <see cref="CreateFilterButton"/>'s click handler creates or updates, and
    /// which identity/label it shows (<c>create-filter</c> vs <c>save-filter</c>
    /// — never both at once, mirroring the linux/web create⇄save toggle).</summary>
    private long? _editingFilterId;

    /// <summary>e2e-only: a one-shot artificial delay (ms) applied at the start of
    /// <see cref="LoadFiltersAsync"/> so a nav-readiness regression test can
    /// DETERMINISTICALLY widen the initial load window (set via the
    /// <c>email_filter_load_delay_ms</c> test command; see <c>App</c>.HandleTestCommand).
    /// Consumed — reset to 0 — by the next load. Zero in production.</summary>
    internal static int TestLoadDelayMs;

    // ── e2e nav-readiness barrier ──
    // Completed at the END of the initial LoadFiltersAsync so a hosting page can gate its
    // own IAsyncLoadedPage barrier on it — otherwise the windows TestAgent flips
    // ready=true right after the frame-nav kickoff, while this panel's own async
    // EmailFiltersListAsync round-trip (and the EditVisibility it computes) is still in
    // flight, and an immediately-following filter_edit_visible() read sees a stale/empty
    // list. RunContinuationsAsynchronously so completing it never resumes the
    // awaiting agent inline on the UI thread.
    private TaskCompletionSource _loadComplete =
        new(TaskCreationOptions.RunContinuationsAsynchronously);
    internal Task LoadComplete => _loadComplete.Task;

    public EmailFilterPanel()
    {
        this.InitializeComponent();
        FilterList.ItemsSource = _filters;
    }

    private async void Panel_Loaded(object sender, RoutedEventArgs e)
    {
        await LoadFiltersAsync();
    }

    private async Task LoadFiltersAsync()
    {
        // e2e-only one-shot slow-load injection (TestLoadDelayMs): keeps the filter list
        // stale for `testDelayMs` ms so a nav-readiness regression can prove an immediate
        // post-navigate read is honoured. Consumed here regardless of NestClient, so a
        // deliberately-armed delay is never silently dropped by an early return.
        var testDelayMs = TestLoadDelayMs;
        TestLoadDelayMs = 0;

        if (NestClient is null) { _loadComplete.TrySetResult(); return; }

        LoadingRing.IsActive = true;
        LoadingRing.Visibility = Visibility.Visible;
        try
        {
            if (testDelayMs > 0) await Task.Delay(testDelayMs);
            // NO ConfigureAwait(false): the continuation mutates `_filters` (bound to
            // FilterList.ItemsSource) + LoadingRing — off-thread bound-state mutation
            // throws a silent COMException (reference_windows_vm_configureawait_comexception),
            // which the catch below swallows, leaving the list empty (filter-item count 0).
            var filters = await NestClient.EmailFiltersListAsync();
            // Re-read the review marks on every list load, BEFORE painting rows —
            // the list paints far more often than the ledger changes, but a Keep
            // answered on another device must still clear here on the next visit.
            // A failed read keeps the cached marks (never "nothing flagged").
            await InheritedFilterMarks.RefreshAsync(NestClient);
            _filters.Clear();
            foreach (var f in filters)
            {
                _filters.Add(new FilterItem
                {
                    Id = f.id,
                    Name = f.name,
                    Action = S.Resolve(FaunaFfiMethods.EmailFilterActionLabel(f.action)),
                    Rules = f.rules,
                    ActionValue = f.action,
                    EditVisibility = FaunaFfiMethods.EmailFilterIsEditableFor(f.rules, f.action, SupportedActionKinds)
                        ? Visibility.Visible
                        : Visibility.Collapsed,
                    MarkVisibility = InheritedFilterMarks.Contains(f.id)
                        ? Visibility.Visible
                        : Visibility.Collapsed,
                });
            }
        }
        catch
        {
            // Silently ignore load errors — page-level error bar is not accessible from here
        }
        finally
        {
            LoadingRing.IsActive = false;
            LoadingRing.Visibility = Visibility.Collapsed;
            // The initial load has rendered (success or handled error): release the
            // nav-readiness barrier. ALWAYS in finally so a hosting page's bounded await
            // never hangs a navigation. A later refresh (e.g. CreateFilter_Click's own
            // reload) just re-completes an already-completed TCS — a harmless no-op.
            _loadComplete.TrySetResult();
        }
    }

    private async void CreateFilter_Click(object sender, RoutedEventArgs e)
    {
        if (NestClient is null) return;

        var name = FilterNameInput.Text.Trim();
        var ruleValue = FilterRuleValue.Text.Trim();
        if (string.IsNullOrEmpty(name) || string.IsNullOrEmpty(ruleValue)) return;

        var ruleType = (FilterRuleType.SelectedItem as ComboBoxItem)?.Tag as string ?? "SenderIs";

        // Adopt the shared form→typed-variant encoders (fauna_protocol::email::
        // encode_filter_{rule,action}, UniFFI-exported) instead of hand-rolling the map
        // (priority #2/#4). The whole FfiFilterActionInputs is the form state: an empty
        // reject reason → the canonical default; a Forward's destination is validated by
        // the nest's own rule-path predicate; an unknown kind or invalid destination
        // throws — bail like the panel's other silent errors.
        FfiEmailFilterRule rule;
        FfiEmailFilterAction action;
        try
        {
            rule = FaunaFfiMethods.EncodeEmailFilterRule(ruleType, ruleValue);
            action = FaunaFfiMethods.EncodeEmailFilterActionInputs(ReadActionInputs());
        }
        catch (FfiException)
        {
            return;
        }

        var editingId = _editingFilterId;

        CreateRing.IsActive = true;
        CreateRing.Visibility = Visibility.Visible;
        CreateFilterButton.IsEnabled = false;
        try
        {
            // NO ConfigureAwait(false): the continuation touches bound UI (the text
            // inputs, the toggle, the form visibility) + reloads the list — off-thread
            // it COMExceptions silently, so the created/saved filter never appears.
            if (editingId is { } id)
            {
                await NestClient.EmailFiltersUpdateAsync(id, name, new[] { rule }, "all", action, 0);
            }
            else
            {
                await NestClient.EmailFiltersCreateAsync(name, new[] { rule }, "all", action, 0);
            }

            FilterNameInput.Text = string.Empty;
            FilterRuleValue.Text = string.Empty;
            WriteActionInputs(new FfiFilterActionInputs("Allow", string.Empty, string.Empty, true));
            AddFilterToggle.IsChecked = false;
            AddFilterForm.Visibility = Visibility.Collapsed;
            SetCreateMode();
            await LoadFiltersAsync();
            // Scroll the list back into the viewport. The email-filters section sits
            // at the bottom of a page taller than the ScrollViewer (SettingsPrivacyPage),
            // so revealing the create/edit form (focus-scroll on its inputs) pushes
            // FilterList itself offscreen-above — mirrors SaveSpamPrefs_Click's identical
            // fix in this same page. Without this, a freshly created/edited filter's row
            // (and its filter-edit/filter-delete buttons) reports is_visible=false even
            // though it rendered correctly; also surfaces the result to a real user.
            // Deferred one LayoutUpdated tick: LoadFiltersAsync's ObservableCollection
            // change hasn't measured/arranged the (possibly new) row yet on this same
            // continuation, so a synchronous StartBringIntoView() here is a no-op against
            // FilterList's pre-reload bounds (mirrors FoldersPage's identical Expander
            // deferral — reference_windows_e2e_is_visible_offscreen).
            void OnListLayout(object? s, object le)
            {
                FilterList.LayoutUpdated -= OnListLayout;
                FilterList.StartBringIntoView();
            }
            FilterList.LayoutUpdated += OnListLayout;
        }
        catch
        {
            // Silently ignore — caller should wire an error bar if needed
        }
        finally
        {
            CreateRing.IsActive = false;
            CreateRing.Visibility = Visibility.Collapsed;
            CreateFilterButton.IsEnabled = true;
        }
    }

    private async void EditFilter_Click(object sender, RoutedEventArgs e)
    {
        if (NestClient is null) return;
        if (sender is not Button btn) return;
        if (btn.DataContext is not FilterItem filter) return;

        try
        {
            // Fresh fetch — never the cached row — so the form pre-populates off
            // exactly what a concurrent edit (another client) last saved.
            var fresh = await NestClient.EmailFiltersGetAsync(filter.Id);
            if (fresh.rules.Length != 1) return; // shouldn't happen given the row's edit gate

            var describedRule = FaunaFfiMethods.DescribeEmailFilterRule(fresh.rules[0]);
            var describedAction = FaunaFfiMethods.DescribeEmailFilterActionInputs(fresh.action);
            if (describedRule is null || describedAction is null) return; // same — the gate should have hidden edit

            FilterNameInput.Text = fresh.name;
            FilterRuleValue.Text = describedRule.value;
            SelectComboItemByTag(FilterRuleType, describedRule.kind);
            WriteActionInputs(describedAction);

            SetEditMode(fresh.id);
            AddFilterToggle.IsChecked = true;
            AddFilterForm.Visibility = Visibility.Visible;
        }
        catch
        {
            // Silently ignore — matches the panel's other silent-error handling
        }
    }

    /// <summary>The Reject reason a loaded filter carried. The form has no field
    /// for it, but it rides along on an edit (settings.md § Email filter
    /// create-dialog encoding: the whole inputs struct is the form state) so
    /// saving a Reject rule never rewrites its reason; empty → the encoder's
    /// canonical default.</summary>
    private string _rejectReason = string.Empty;

    /// <summary>The form's action inputs as the shared encoder takes them.</summary>
    private FfiFilterActionInputs ReadActionInputs()
    {
        var kind = (FilterActionSelect.SelectedItem as ComboBoxItem)?.Tag as string ?? "Allow";
        return new FfiFilterActionInputs(
            kind,
            _rejectReason,
            FilterForwardAddressInput.Text.Trim(),
            FilterKeepLocalCopyBox.IsChecked ?? true);
    }

    /// <summary>Load <paramref name="inputs"/> into the form (the edit pre-populate,
    /// and the create form's reset with the default inputs).</summary>
    private void WriteActionInputs(FfiFilterActionInputs inputs)
    {
        _rejectReason = inputs.rejectReason;
        SelectComboItemByTag(FilterActionSelect, inputs.kind);
        FilterForwardAddressInput.Text = inputs.forwardAddress;
        FilterKeepLocalCopyBox.IsChecked = inputs.keepLocalCopy;
    }

    /// <summary>The Forward inputs show only while the action reads Forward.
    /// Fires once during <c>InitializeComponent</c> (the default item's
    /// <c>IsSelected</c>), before the named inputs exist — hence the null guard.</summary>
    private void FilterActionSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (ForwardInputs is null) return;
        var kind = (FilterActionSelect.SelectedItem as ComboBoxItem)?.Tag as string;
        ForwardInputs.Visibility = kind == "Forward" ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>Select the <see cref="ComboBoxItem"/> whose <c>Tag</c> equals
    /// <paramref name="tag"/> (the wire enum kind — same key space
    /// <see cref="CreateFilter_Click"/> reads back out of <c>SelectedItem</c>).</summary>
    private static void SelectComboItemByTag(ComboBox combo, string tag)
    {
        foreach (var item in combo.Items)
        {
            if (item is ComboBoxItem cbi && (cbi.Tag as string) == tag)
            {
                combo.SelectedItem = cbi;
                return;
            }
        }
    }

    /// <summary>Switch <see cref="CreateFilterButton"/> to save-mode for filter
    /// <paramref name="id"/> — its identity AND label flip (<c>create-filter</c> →
    /// <c>save-filter</c>), never both showing at once.</summary>
    private void SetEditMode(long id)
    {
        _editingFilterId = id;
        AutomationProperties.SetAutomationId(CreateFilterButton, Ids.SaveFilter);
        CreateFilterButton.Content = S.Get("common/save");
    }

    /// <summary>Switch <see cref="CreateFilterButton"/> back to create-mode — the
    /// inverse of <see cref="SetEditMode"/>. Idempotent; safe to call even when
    /// already in create mode (the toggle's collapse path calls it unconditionally).</summary>
    private void SetCreateMode()
    {
        _editingFilterId = null;
        AutomationProperties.SetAutomationId(CreateFilterButton, Ids.CreateFilter);
        CreateFilterButton.Content = S.Get("common/create");
    }

    private async void DeleteFilter_Click(object sender, RoutedEventArgs e)
    {
        if (NestClient is null) return;
        if (sender is not Button btn) return;
        if (btn.DataContext is not FilterItem filter) return;

        try
        {
            // NO ConfigureAwait(false): `_filters.Remove` mutates the bound collection
            // (off-thread → silent COMException).
            var wasMarked = InheritedFilterMarks.Contains(filter.Id);
            await NestClient.EmailFiltersDeleteAsync(filter.Id);
            _filters.Remove(filter);
            if (wasMarked)
            {
                // Delete IS the review's Remove half: record the verdict only
                // AFTER the rule is gone — a failure here leaves a re-asked
                // question, never a silenced armed rule.
                await NestClient.FilterMarkRemovedAsync(filter.Id);
                await InheritedFilterMarks.RefreshAsync(NestClient);
            }
        }
        catch
        {
            // Silently ignore
        }
    }

    /// <summary>The review's Keep: record the verdict through the shared decider,
    /// then reload so the row's mark clears (and the Account line with it — both
    /// read <see cref="InheritedFilterMarks"/>).</summary>
    private async void KeepFilter_Click(object sender, RoutedEventArgs e)
    {
        if (NestClient is null) return;
        if (sender is not Button btn) return;
        if (btn.DataContext is not FilterItem filter) return;

        try
        {
            // NO ConfigureAwait(false): the reload mutates the bound collection.
            await NestClient.FilterMarkKeepAsync(filter.Id);
            await LoadFiltersAsync();
        }
        catch
        {
            // Silently ignore — matches the panel's other silent-error handling
        }
    }

    // Checked/Unchecked (not Click): the e2e bridge actuates a ToggleButton via
    // its UIA TogglePattern, and WinUI's automation Toggle() flips IsChecked +
    // raises Checked/Unchecked but does NOT raise Click — so a Click handler never
    // fired and the form stayed collapsed. Checked/Unchecked fire for both the
    // programmatic toggle and a real user click. Matches DmComposeBar's pattern.
    private void AddFilterToggle_Toggled(object sender, RoutedEventArgs e)
    {
        var isChecked = AddFilterToggle.IsChecked ?? false;
        AddFilterForm.Visibility = isChecked ? Visibility.Visible : Visibility.Collapsed;
        if (!isChecked)
        {
            // Collapsing (including a cancelled edit) always drops back to
            // create-mode — never leaves CreateFilterButton stuck showing
            // save-filter with no form open to save from.
            SetCreateMode();
        }
    }

}
