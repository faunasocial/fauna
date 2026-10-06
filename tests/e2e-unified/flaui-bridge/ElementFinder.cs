using System.Linq;
using FlaUI.Core.AutomationElements;
using FlaUI.Core.Conditions;

namespace FauiBridge;

/// <summary>
/// One step in a scoped element query path. Wire shape is
/// <c>{"id": "...", "index": N}</c>; deserialized by Program.cs from
/// JSON list-of-objects passed as either a <c>scope</c> query param
/// (GET) or JSON body field (POST).
/// </summary>
record ScopeStep(string Id, int Index = 0);

/// <summary>Where the one in-flight UIA call currently is.
///
/// The per-strategy narration in <see cref="ElementFinder"/> and the resolve
/// narration in <see cref="SessionManager"/> both report from a <c>finally</c>, so
/// they say nothing at all about a call that NEVER RETURNS — which is precisely the
/// failure being chased: a single <c>GET /element/visible</c> was measured holding
/// the UIA gate for over ten minutes while every one of those instruments stayed
/// silent. A breadcrumb written BEFORE each call, readable by the heartbeat thread,
/// is the only way to name a call from outside while it is still stuck in it.
///
/// Safe to write from the request thread and read from the heartbeat: one plain
/// reference field through <see cref="Volatile"/>, no lock the stuck thread could
/// be holding. The UIA gate means there is only ever one writer.</summary>
static class UiaStage
{
    static string _stage = "(none)";

    public static string Current => Volatile.Read(ref _stage);

    public static void Enter(string stage) => Volatile.Write(ref _stage, stage);
}

static class ElementFinder
{
    /// <summary>
    /// Walk a scope chain from <paramref name="root"/>: locate scope[0]
    /// among descendants, then scope[1] inside that, etc. Returns the
    /// final scoped element. If <paramref name="scope"/> is null or
    /// empty, returns <paramref name="root"/> unchanged so callers can
    /// pass scope through unconditionally.
    /// </summary>
    public static AutomationElement WalkScope(AutomationElement root, ConditionFactory cf,
        IReadOnlyList<ScopeStep>? scope)
    {
        if (scope is null || scope.Count == 0) return root;
        var current = root;
        foreach (var step in scope)
        {
            current = FindOne(current, cf, step.Id, step.Index);
        }
        return current;
    }


    /// <summary>A find slower than this narrates its per-strategy breakdown on
    /// stderr. Sized well above a healthy find (a hit on strategy 1 is
    /// milliseconds; a full five-strategy miss on a big page is under a second
    /// on an idle box) and well under the driver's own 120s HTTP budget, so the
    /// line always lands BEFORE the request is abandoned — the whole point is
    /// that a peg says what it was pegged on rather than dying silently.</summary>
    private const double SlowFindNarrateSeconds = 5.0;

    /// <summary>
    /// Find elements by AutomationId, with Name fallback.
    /// For *-tab IDs, opens the NavigationView pane and searches by display Name.
    /// Also searches popup/dialog windows (ContentDialog hosts a separate top-level
    /// window in WinUI 3 desktop apps that is not a descendant of the main window).
    ///
    /// Every UIA call below is served by the target app's UI thread, so a find can
    /// only be as fast as that thread is free. When one runs long this narrates the
    /// per-strategy breakdown to stderr (drained into the driver's bridge log — see
    /// <c>drivers/windows.py::bridge_log</c>), because the failure it feeds is
    /// otherwise mute: the driver reports "bridge thread pegged" with no idea WHICH
    /// call pegged it, and a miss runs five strategies with two whole-tree walks
    /// among them. e2e convention 6 — a failure must diagnose itself.
    /// </summary>
    public static AutomationElement[] FindAll(AutomationElement root, ConditionFactory cf, string id)
    {
        var narration = new List<string>();
        var whole = System.Diagnostics.Stopwatch.StartNew();
        try
        {
            return FindAllNarrated(root, cf, id, narration);
        }
        finally
        {
            whole.Stop();
            if (whole.Elapsed.TotalSeconds >= SlowFindNarrateSeconds)
            {
                Console.Error.WriteLine(
                    $"[bridge] SLOW FIND '{id}' took {whole.Elapsed.TotalSeconds:N1}s " +
                    $"({string.Join(", ", narration)}) — every step here is served by the " +
                    "app's UI thread, so a long one means that thread was busy, not that " +
                    "the tree is big");
            }
        }
    }

    /// <summary>Time one strategy into <paramref name="narration"/> and return its
    /// hits. The label is what the SLOW FIND line names, so keep it short and
    /// specific enough to point at a call site.</summary>
    private static AutomationElement[] Step(List<string> narration, string label,
        Func<AutomationElement[]> run)
    {
        var sw = System.Diagnostics.Stopwatch.StartNew();
        UiaStage.Enter($"ElementFinder.{label}");
        try
        {
            return run();
        }
        finally
        {
            sw.Stop();
            UiaStage.Enter($"ElementFinder.{label} (returned)");
            narration.Add($"{label}={sw.Elapsed.TotalSeconds:N1}s");
        }
    }

    private static AutomationElement[] FindAllNarrated(AutomationElement root,
        ConditionFactory cf, string id, List<string> narration)
    {
        // Scoped sub-element search: when `root` is NOT a top-level window (it's a
        // scoped element a `WalkScope` step resolved — a post-card Grid, a quoted-post
        // Border, …), WinUI 3 UIA's `FindAllDescendants` ignores the root scope and
        // returns whole-window matches (a known automation-peer limitation, worse for
        // Border/Grid peers). That made a scoped `is_visible`/`count` escape its
        // subtree — e.g. `count("unverified-source-badge", scope="post-card[1]")`
        // returned the badge living in `post-card[0]`. Re-impose the scope client-side
        // by filtering descendants to `root`'s actual subtree (parent-walk), and skip
        // the window-level fallbacks below (popups, nav-pane `-tab`, global Name) —
        // none of those make sense from inside a scoped element.
        if (!IsWindowRoot(root))
        {
            var scopedById = Step(narration, "scoped-by-id",
                () => WithinSubtree(root.FindAllDescendants(cf.ByAutomationId(id)), root));
            if (scopedById.Length > 0)
                return scopedById;
            // Name fallback, still confined to the subtree (an element the producer
            // gave a Name but no AutomationId, e.g. a -tab is never scoped here).
            return Step(narration, "scoped-by-name",
                () => WithinSubtree(root.FindAllDescendants(cf.ByName(id)), root));
        }

        // Strategy 1: AutomationId on the main window
        var byAutoId = Step(narration, "s1-window-by-id",
            () => root.FindAllDescendants(cf.ByAutomationId(id)));
        if (byAutoId.Length > 0)
            return byAutoId;

        // Strategy 2: AutomationId on dialog/popup windows owned by the same process.
        // WinUI 3 ContentDialog renders into a Popup whose visual host is a separate
        // top-level window ("Popup Host" / "Microsoft.UI.Content.PopupWindowSiteBridge").
        var popupHits = Step(narration, "s2-popups-by-id",
            () => SearchPopups(root, cf, byId: true, term: id));
        if (popupHits.Length > 0)
            return popupHits;

        // Strategy 3: For nav tabs, search by display Name (may need pane open)
        if (id.EndsWith("-tab"))
        {
            var displayName = TabIdToDisplayName(id);
            if (displayName is not null)
            {
                // Try finding by Name first (pane might already be open)
                var byName = Step(narration, "s3-tab-by-name",
                    () => root.FindAllDescendants(cf.ByName(displayName)));
                if (byName.Length > 0)
                    return byName;

                // Pane might be closed — toggle it open and retry
                byName = Step(narration, "s3-toggle-pane-retry", () =>
                {
                    ToggleNavPane(root, cf);
                    return root.FindAllDescendants(cf.ByName(displayName));
                });
                if (byName.Length > 0)
                    return byName;
            }
        }

        // Strategy 4: General Name fallback (main window)
        var byNameGeneral = Step(narration, "s4-window-by-name",
            () => root.FindAllDescendants(cf.ByName(id)));
        if (byNameGeneral.Length > 0)
            return byNameGeneral;

        // Strategy 5: Name fallback on popup windows
        var popupNameHits = Step(narration, "s5-popups-by-name",
            () => SearchPopups(root, cf, byId: false, term: id));
        if (popupNameHits.Length > 0)
            return popupNameHits;

        return [];
    }

    /// <summary>
    /// Walk every top-level window in the same process as the main app window and
    /// search each for the term. Used to reach ContentDialog content which lives
    /// in a separate popup host window.
    /// </summary>
    private static AutomationElement[] SearchPopups(AutomationElement mainWindow,
        ConditionFactory cf, bool byId, string term)
    {
        try
        {
            var automation = mainWindow.Automation;
            var desktop = automation.GetDesktop();
            var pid = mainWindow.Properties.ProcessId.ValueOrDefault;
            var siblings = desktop.FindAllChildren();
            var hits = new List<AutomationElement>();
            foreach (var sib in siblings)
            {
                if (sib.Equals(mainWindow)) continue;
                int sibPid = 0;
                try { sibPid = sib.Properties.ProcessId.ValueOrDefault; } catch { continue; }
                if (sibPid != pid) continue;
                var found = byId
                    ? sib.FindAllDescendants(cf.ByAutomationId(term))
                    : sib.FindAllDescendants(cf.ByName(term));
                if (found.Length > 0) hits.AddRange(found);
            }
            return hits.ToArray();
        }
        catch
        {
            return [];
        }
    }

    public static AutomationElement FindOne(AutomationElement root, ConditionFactory cf, string id, int index)
    {
        var all = FindAll(root, cf, id);
        if (index >= all.Length)
            throw new ElementNotFoundException(id, index, all.Length);
        return all[index];
    }

    /// <summary>
    /// Read an element's Name without throwing PropertyNotSupported [#30005].
    /// ListViewItem, Border, and plain Grid peers don't populate Name;
    /// FlaUI's direct .Name getter raises, which cascades into bridge 500s.
    /// </summary>
    private static string SafeName(AutomationElement el)
    {
        try { return el.Name ?? ""; }
        catch { return ""; }
    }

    /// <summary>
    /// Get text from an element, with InfoBar child TextBlock fallback.
    /// </summary>
    public static string GetText(AutomationElement el, ConditionFactory cf)
    {
        // Value pattern (TextBox, etc.)
        if (el.Patterns.Value.IsSupported)
        {
            var val = el.Patterns.Value.Pattern.Value.Value;
            if (!string.IsNullOrEmpty(val))
                return val;
            // An Edit control's ValuePattern is authoritative even when empty. Falling
            // through would scrape the template's placeholder TextBlock (WinUI also fills
            // the peer's Name from PlaceholderText) and report the PLACEHOLDER as the
            // field's content — every other app reads an empty field back as "".
            if (el.Properties.ControlType.ValueOrDefault == FlaUI.Core.Definitions.ControlType.Edit)
                return "";
        }

        // Selection pattern (ComboBox / Selector): return the selected item's text. A
        // non-editable WinUI ComboBox exposes its current pick here, not via Value — and
        // the tier pickers' get_text contract (admin.py _pick_tier reads it to know the
        // selection reached target) needs the selected item, not scraped child TextBlocks.
        if (el.Patterns.Selection.IsSupported)
        {
            var selected = el.Patterns.Selection.Pattern.Selection.ValueOrDefault;
            if (selected is { Length: > 0 })
            {
                var selName = SafeName(selected[0]);
                if (!string.IsNullOrEmpty(selName))
                    return selName;
            }
        }

        // InfoBar: look for child with AutomationId="Message"
        var msgChild = el.FindFirstDescendant(cf.ByAutomationId("Message"));
        if (msgChild is not null)
        {
            var msgName = SafeName(msgChild);
            if (!string.IsNullOrEmpty(msgName))
                return msgName;
        }

        // Collect text from all child TextBlock elements
        var textChildren = el.FindAllDescendants(cf.ByClassName("TextBlock"));
        var parts = new List<string>();
        foreach (var child in textChildren)
        {
            var childName = SafeName(child);
            // Skip icon glyphs (single chars from Segoe MDL2 Assets)
            if (!string.IsNullOrEmpty(childName) && childName.Length > 1)
                parts.Add(childName);
        }
        if (parts.Count > 0)
            return string.Join(" ", parts);

        // Direct Name property (last resort — safe even on container peers)
        var name = SafeName(el);
        if (!string.IsNullOrEmpty(name))
            return name;

        return "";
    }

    internal static void ToggleNavPane(AutomationElement root, ConditionFactory cf)
    {
        var toggle = root.FindFirstDescendant(cf.ByAutomationId("TogglePaneButton"));
        if (toggle is null) return;
        // Prefer InvokePattern — a COM call into the provider, foreground-independent
        // — over the physical click FlaUI's own Click() uses, which requires an
        // ATTACHED interactive desktop and fails Win32Exception(5) "Access is
        // denied" without one. Found
        // (the `Actions.cs` SendInput sweep): this site sits outside that file but
        // hits the identical wall — measured live, `test_windows_account_switcher_
        // lists_switches_and_reveals_admin` failing here on a Disc session.
        // Physical click stays the fallback for a build where the toggle button
        // somehow lacks Invoke (unwrapped, matching this call's prior behavior —
        // best-effort element-finding scaffolding, not a gesture with a caller to
        // diagnose for).
        if (toggle.Patterns.Invoke.IsSupported)
        {
            toggle.Patterns.Invoke.Pattern.Invoke();
        }
        else
        {
            toggle.Click();
        }
        Thread.Sleep(500);
    }

    private static readonly Dictionary<string, string> TabNameMap = new()
    {
        ["feed-tab"] = "Feed",
        ["conversations-tab"] = "Conversations",
        ["contacts-tab"] = "Contacts",
        ["profile-tab"] = "Profile",
        ["groups-tab"] = "Groups",
        ["bridges-tab"] = "Bridges",
        ["backups-tab"] = "Backups",
        ["media-tab"] = "Media",
        ["search-tab"] = "Search",
        ["events-tab"] = "Calendar",
        ["moderation-tab"] = "Moderation",
        ["p2p-tab"] = "P2P",
        ["notifications-tab"] = "Notifications",
        // sync-tab + conflicts-tab RETIRED (2026-06-28 sync/folder UI unification):
        // the roster moved to Settings → Devices and the folder control plane +
        // conflicts to Settings → Folders; both reached via the settings sub-page nav.
        ["settings-tab"] = "Settings",
        ["status-tab"] = "Status",
        // The two GATED tabs (admin.md § Navigation model; family-safety.md
        // § App surface) — they were the only `-tab`s missing from this table, so
        // completing it is right regardless.
        // ⚠ CORRECTED 2026-07-19: the note that used to sit here — "a gated
        // NavigationViewItem revealed at runtime never enters the UIA tree at all" —
        // was WRONG. Both resolve by AutomationId perfectly well (measured:
        // count("admin-tab") == 1 for an admin). They were merely laid out BELOW THE
        // FOLD of a nav pane taller than the window, so `is_visible` (which reads UIA
        // IsOffscreen) returned false while count returned 1. Tests use
        // driver.is_visible_scrolled(); nothing here needed changing.
        // ⚠ Adding a new nav tab to MainPage.xaml means adding it HERE too.
        ["admin-tab"] = "Admin",
        ["family-tab"] = "Family",
    };

    private static string? TabIdToDisplayName(string id)
    {
        return TabNameMap.GetValueOrDefault(id);
    }

    /// <summary>
    /// Whether <paramref name="el"/> is a top-level window — the discriminator for
    /// the scoped-vs-window search split in <see cref="FindAll"/>. The app's main
    /// window (and any ContentDialog popup host) carries a real Win32 handle; an
    /// in-app scoped element (Grid/Border/StackPanel a scope step resolves) is
    /// windowless (handle 0). Keyed off the same <c>NativeWindowHandle</c> the
    /// disabled-ancestor walk in <c>Actions.GetAttr</c> uses.
    /// </summary>
    private static bool IsWindowRoot(AutomationElement el)
    {
        try { return (IntPtr)el.Properties.NativeWindowHandle.ValueOrDefault != IntPtr.Zero; }
        catch { return false; }
    }

    /// <summary>Keep only the <paramref name="hits"/> that actually live inside
    /// <paramref name="root"/>'s subtree (re-imposing the scope WinUI 3 UIA drops —
    /// see <see cref="FindAll"/>).</summary>
    private static AutomationElement[] WithinSubtree(AutomationElement[] hits, AutomationElement root)
        => hits.Where(h => IsWithin(h, root)).ToArray();

    /// <summary>
    /// Walk <paramref name="el"/>'s parent chain (itself first) up to a bounded
    /// depth; true iff <paramref name="ancestor"/> is reached. The robust client-side
    /// re-scope for the WinUI 3 <c>FindAllDescendants</c> escape — independent of
    /// whatever makes UIA ignore the root scope.
    /// </summary>
    private static bool IsWithin(AutomationElement el, AutomationElement ancestor)
    {
        var cur = el;
        for (int i = 0; i < 64 && cur is not null; i++)
        {
            if (SameElement(cur, ancestor)) return true;
            try { cur = cur.Parent; }
            catch { return false; }
        }
        return false;
    }

    /// <summary>UIA-identity compare via <c>RuntimeId</c> (the canonical per-session
    /// element identity), falling back to FlaUI's <c>Equals</c> if RuntimeId is
    /// unavailable.</summary>
    private static bool SameElement(AutomationElement a, AutomationElement b)
    {
        try
        {
            var ra = a.Properties.RuntimeId.ValueOrDefault;
            var rb = b.Properties.RuntimeId.ValueOrDefault;
            if (ra is not null && rb is not null)
                return ra.SequenceEqual(rb);
        }
        catch { }
        return a.Equals(b);
    }
}

class ElementNotFoundException : Exception
{
    public ElementNotFoundException(string id, int index, int found)
        : base($"Element '{id}' index {index} not found (found {found})") { }
}
