using FlaUI.Core.AutomationElements;
using FlaUI.Core.Input;
using FlaUI.Core.Capturing;
using FlaUI.Core.WindowsAPI;

namespace FauiBridge;

class Actions
{
    private readonly SessionManager _session;

    /// <summary>Where an action reports its own per-phase costs, wired by
    /// <c>Program</c> to the diag file (never stderr: a diagnostic that can block
    /// on a full pipe is worse than none). Null off a bridge run — the unit
    /// entry points construct <see cref="Actions"/> directly.
    ///
    /// <para>Exists because "this request held the UIA gate for 21 s" names the
    /// request but not the phase, and an action that makes dozens of cross-process
    /// UIA calls has many phases. Three <c>scroll-into-view</c> holds of 17 s, 25 s
    /// and 21 s recurred with near-identical timings across five separate runs —
    /// too regular for load jitter, which is what pointed at a fixed step
    /// count — but only a per-phase
    /// number can say whether the cost is the tree walk, the scroll calls, or the
    /// visibility reads between them.</para></summary>
    public static Action<string>? Trace;

    private static void T(string message)
    {
        try { Trace?.Invoke(message); } catch { /* diagnostics never break an action */ }
    }

    /// <summary>Convention 11's actuation gate for this session — an actuation
    /// route must not drive a control the UI has disabled. Never null: a caller
    /// that supplies none gets windows' default stance
    /// (<see cref="ActuationGate.WindowsRefusesDisabledActuationByDefault"/>), which
    /// is what the unit entry points and the self-test construct.</summary>
    private readonly ActuationGate _gate;

    public Actions(SessionManager session, ActuationGate? gate = null)
    {
        _session = session;
        _gate = gate ?? ActuationGate.FromEnvironment(null);
    }

    /// <summary>The element's LIVE enabled state, as UIA sees it right now.
    ///
    /// <para>UIA's <c>IsEnabled</c> is already the EFFECTIVE, ancestor-inclusive
    /// read — a control greyed only by a disabled ancestor reads disabled here —
    /// so windows needs no <c>folding</c> analogue, for the same reason GTK's
    /// <c>is_sensitive()</c> spared linux one.</para>
    ///
    /// <para><b>An unreadable property is ENABLED, never disabled.</b> This read is
    /// a cross-process COM call and can throw for reasons that have nothing to do
    /// with the control's state (an element that vanished mid-call, a busy UI
    /// thread). Refusing on a failed read would turn a transport hiccup into a
    /// named "the UI disabled this" verdict — a false diagnosis, and on the hot
    /// path of every click the harness issues. It also matches the convention's
    /// own default: an entry that registers no predicate is enabled.</para></summary>
    private static bool LiveEnabled(AutomationElement el)
    {
        try { return el.IsEnabled; }
        catch { return true; }
    }

    /// <summary>Consult the gate before an actuation route drives
    /// <paramref name="el"/>. Throws <see cref="DisabledActuationException"/> in
    /// strict mode; records a marker and returns in permissive mode.</summary>
    private void Gate(string route, string id, int index, AutomationElement el)
        => _gate.Check(route, id, index, LiveEnabled(el));

    public void Click(string id, int index = 0, IReadOnlyList<ScopeStep>? scope = null)
    {
        // Prefer InvokePattern / Toggle / SelectionItem — these don't require the
        // target window to be foreground. FlaUI's physical Click() uses mouse
        // SendInput which fails with UIA "Access is denied" when the FaunaApp
        // window is behind another app (common in long test runs on a shared
        // desktop). Fall back to physical click only when no invocation pattern
        // is available.
        //
        // Retry-on-disabled: a previous click may have toggled state that
        // re-enables this element (e.g. unchecking dns-same-provider-checkbox
        // re-enables DNS-only provider buttons). The 150ms post-click sleep
        // below isn't always enough — the chain checkbox-toggle → TwoWay
        // binding → VM setter → machine mutation → observer tick →
        // PropertyChanged → re-evaluation of IsEnabled bindings can stretch
        // past 150ms when the view tree is large. Re-find + retry up to
        // 750ms total handles the race; a target that stays disabled is a
        // genuine test bug and surfaces as the same exception just later.
        var deadline = DateTime.UtcNow + TimeSpan.FromMilliseconds(750);
        while (true)
        {
            var el = Find(id, index, scope);
            // Convention 11's gate rides the SAME window the retry above describes,
            // rather than firing on the first disabled read. That is not leniency:
            // the retry exists because a legitimate chain (toggle → TwoWay binding →
            // VM setter → machine mutation → observer tick → PropertyChanged →
            // IsEnabled re-evaluation) can leave a control that IS about to enable
            // reading disabled for a few hundred ms, and a gate that refused there
            // would red hundreds of honest tests while reporting them as convention-11
            // violations — the over-broad-predicate failure this rollout must not
            // cause. A control still disabled when the window closes is the genuine
            // article, and the exception the old comment promised "just later" is now
            // the named 409 instead.
            if (!LiveEnabled(el))
            {
                if (DateTime.UtcNow < deadline) { Thread.Sleep(50); continue; }
                // Strict: throws. Permissive: marks it and falls through to drive the
                // control exactly as before, so the sweep's baseline is unchanged.
                _gate.Check("click", id, index, enabled: false);
            }
            try
            {
                if (el.Patterns.Invoke.IsSupported)
                {
                    WatchForeground($"Invoke on {Describe(el)}",
                        () => el.Patterns.Invoke.Pattern.Invoke());
                }
                else if (el.Patterns.Toggle.IsSupported)
                {
                    WatchForeground($"Toggle on {Describe(el)}",
                        () => el.Patterns.Toggle.Pattern.Toggle());
                }
                else if (el.Patterns.SelectionItem.IsSupported)
                {
                    WatchForeground($"SelectionItem.Select on {Describe(el)}",
                        () => el.Patterns.SelectionItem.Pattern.Select());
                }
                else if (el.Patterns.ExpandCollapse.IsSupported)
                {
                    // A WinUI Expander (e.g. folder-row) supports neither Invoke
                    // nor Toggle nor SelectionItem — it exposes ExpandCollapse. Use
                    // it (non-physical, foreground-independent) instead of falling
                    // through to the flaky physical SendInput click below, which
                    // misses the header when the Expander sits in a nested/scrolled
                    // shell (the FoldersPage-inside-the-Settings-shell expander
                    // never expanded → body children "found 0", 2026-06-30; the same
                    // SendInput flake that was merely intermittent on the old
                    // top-level SyncPage). Expand only (idempotent): every e2e use
                    // of a folder-row click is expand-to-reveal-body. Mirrors how
                    // `Select` already drives a ComboBox via ExpandCollapse.
                    el.Patterns.ExpandCollapse.Pattern.Expand();
                }
                else
                {
                    try
                    {
                        // A physical click needs an on-screen point. A pattern-less
                        // leaf inside a scrolled-away list row (a TextBlock in a
                        // ListViewItem: the Events calendar list) has none, and
                        // el.Click() throws NoClickablePointException.
                        // Scroll its row into view first. When the element is
                        // already on screen, this costs a single IsOffscreen read.
                        BringRowIntoView(el);
                        PhysicalOnly($"click on {Describe(el)}", () => el.Click());
                    }
                    catch (FlaUI.Core.Exceptions.NoClickablePointException)
                        when (el.Patterns.Value.IsSupported && !el.Patterns.Value.Pattern.IsReadOnly.ValueOrDefault)
                    {
                        // An editable control (TextBox, …) with no clickable point —
                        // e.g. mid-field right after a `clear_and_type` leaves the
                        // caret with no stable click target. The cross-app "click an
                        // editable to commit" contract (actions/backups.py's
                        // `set_member_cap`, mirroring linux/tui's SpinButton-activate
                        // idiom) means the caller wants the typed value COMMITTED,
                        // not a specific pixel struck — send Enter instead (the
                        // element is already focused from the preceding type), which
                        // every commit-on-Enter/commit-on-blur WinUI handler treats
                        // identically. Physical click stays the FIRST attempt for
                        // every other case (focusing a field, moving the caret) —
                        // this only fires on the exception a plain click already
                        // couldn't recover from.
                        CommitEditable(el);
                    }
                }
                break;
            }
            catch (FlaUI.Core.Exceptions.ElementNotEnabledException)
                when (DateTime.UtcNow < deadline)
            {
                Thread.Sleep(50);
            }
        }
        // Tiny post-click breathing room. WinUI commands declared as
        // `async Task` (the standard CommunityToolkit MVVM RelayCommand
        // pattern) return to UIA synchronously after kicking off the
        // async task; the actual state mutation + observer notification
        // + view-tree update happen on subsequent UI-thread passes. A
        // short sleep here gives the async chain time to flush before
        // the test issues its next assertion (e.g. `is_visible` on the
        // newly-mounted view). Mirrors the implicit "command settle"
        // semantics the web Playwright bridge gets for free from
        // page.click() awaiting the navigation.
        System.Threading.Thread.Sleep(150);
    }

    public void DoubleClick(string id, int index = 0, IReadOnlyList<ScopeStep>? scope = null)
    {
        // The Outlook day-cell double-click → new-event compose (events.md
        // § Layout & flow). No invoke-pattern equivalent for a genuine double
        // click, so this is the physical-input path; the web Playwright `dblclick`
        // is the canonical gate (FlaUI double-click is flaky on win-arm64). Genuinely
        // NOT convertible (, judged): the day-cell click
        // arbiter (events feature) treats a second Invoke as a drill-in re-arm, never
        // as a double — there is no UIA path that means "double click" here.
        var el = Find(id, index, scope);
        // Convention 11: a physical double-click on a disabled control is the
        // silent-honour case at its purest — SendInput strikes the pixel, no
        // handler runs, and the route acks 200.
        Gate("double-click", id, index, el);
        PhysicalOnly($"double-click on {Describe(el)}", () => el.DoubleClick());
        System.Threading.Thread.Sleep(150);
    }

    public void Type(string id, string text, int index = 0, IReadOnlyList<ScopeStep>? scope = null)
    {
        var el = Find(id, index, scope);

        // WinUI CalendarDatePicker / TimePicker can't be set by text and are
        // disturbed by physical typing; no-op (see IsNonTypeablePicker).
        if (IsNonTypeablePicker(el)) return;

        // Convention 11, AFTER the structural check above and before any input:
        // "gating click alone is not compliance — typing into a disabled field is
        // the same illegal act". The physical fallback below is the silent half —
        // SendInput into a disabled TextBox writes nothing and acks 200.
        Gate("type", id, index, el);

        // Prefer ValuePattern.SetValue for the WHOLE text, newlines included.
        // Multi-line controls (RichEditBox / multi-line TextBox) accept '\n' as
        // content, so a multi-line body types in one shot — and this avoids
        // physical key input, which Windows 11 foreground-blocks for the non-
        // foreground bridge process (SendInput → Win32 "Access is denied").
        // Windows tests submit via explicit buttons, never Enter-to-submit (the
        // SendInput sandbox makes physical Enter unusable here anyway), so a
        // '\n' reaching this method is message content, not a submit gesture.
        if (WatchForeground($"ValuePattern.SetValue on {Describe(el)}",
                () => TrySetValueAppend(el, text))) return;
        var inner = FindInnerEditWithRetry(el, TimeSpan.FromSeconds(2));
        if (inner is not null && WatchForeground($"ValuePattern.SetValue on {Describe(inner)}",
                () => TrySetValueAppend(inner, text))) return;

        // Fallback (ValuePattern unsupported on this control): physical typing.
        // Handle '\n' as Enter for the rare ContentDialog-confirmation path —
        // this hits the SendInput sandbox on Windows 11, so the ValuePattern
        // path above is strongly preferred (and is what the conversations
        // compose body relies on).
        if (text.Contains('\n'))
        {
            var segments = text.Replace("\r\n", "\n").Split('\n');
            // The LAST segment carrying content — every Enter at or past this
            // index is a TRAILING confirm (nothing more will be typed after it),
            // and only a trailing Enter can safely go through CommitEditable's
            // UIA focus-shift: that shift moves focus OFF `el` onto a neighbour,
            // which would strand a segment still to come. An Enter that has more
            // typing after it (embedded mid-sequence) stays physical.
            int lastContentIndex = -1;
            for (int j = segments.Length - 1; j >= 0; j--)
            {
                if (segments[j].Length > 0) { lastContentIndex = j; break; }
            }
            // ONE critical section for focus + every segment (see FocusThenInject).
            WithInputLock(() =>
            {
                TakeFocus(el, $"focus for physical typing into {Describe(el)}");
                for (int i = 0; i < segments.Length; i++)
                {
                    if (segments[i].Length > 0)
                        PhysicalOnly($"type into {Describe(el)}", () => Keyboard.Type(segments[i]));
                    if (i < segments.Length - 1)
                    {
                        // : a trailing Enter is exactly
                        // CommitEditable's "commit an already-written value" shape
                        // (the Click fallback's own case), so it prefers the same
                        // UIA focus-shift over physical Enter.
                        if (i >= lastContentIndex)
                            CommitEditable(el);
                        else
                            PhysicalOnly($"Enter mid-typing in {Describe(el)}",
                                () => Keyboard.Press(VirtualKeyShort.ENTER));
                    }
                }
            });
            return;
        }

        WithInputLock(() =>
        {
            TakeFocus(el, $"focus for physical typing into {Describe(el)}");
            PhysicalOnly($"type into {Describe(el)}", () => Keyboard.Type(text));
        });
    }

    /// <summary>
    /// DIAGNOSTIC: type <paramref name="text"/> through the PHYSICAL
    /// <c>SendInput</c> path unconditionally — never the ValuePattern shortcut
    /// <see cref="Type"/> prefers — and, if <paramref name="preDelayMs"/> is set,
    /// hold the input critical section open that long before injecting.
    ///
    /// <para><b>Why this exists.</b> <see cref="_inputMutex"/>'s whole claim is
    /// that a sibling FlaUI session's keystrokes cannot land in OUR app. Proving
    /// that needs two bridges whose physical-input sections deliberately overlap,
    /// and no ordinary driver call reaches <see cref="SendPhysicalInput"/> reliably:
    /// every text control in FaunaApp answers ValuePattern, so <see cref="Type"/>
    /// returns before it ever touches the keyboard, and <see cref="PressKey"/>
    /// carries no printable characters to tell the two apps' input apart. The
    /// <paramref name="preDelayMs"/> hold is what makes the collision DETERMINISTIC
    /// rather than a race the test hopes to lose: the caller pins the interleaving
    /// instead of retrying until it happens.</para>
    ///
    /// <para>Sole consumer: <c>tests/test_flaui_input_lock_windows.py</c>. This is
    /// bridge-side test scaffolding, not an app automation surface — the FlaUI
    /// bridge ships in no artifact (testing.md § convention 15).</para>
    /// </summary>
    public PhysicalTypeReport TypePhysical(
        string id, string text, int index = 0, IReadOnlyList<ScopeStep>? scope = null,
        int preDelayMs = 0)
    {
        var el = Find(id, index, scope);
        // Who actually owned the foreground, measured INSIDE the critical section:
        // once right after SendPhysicalInput's ForegroundApp(), and again at the
        // instant of injection. That pair is the whole diagnosis — it separates
        // "we never won the foreground" from "we won it and a sibling took it back",
        // which need opposite fixes and are indistinguishable from the typed text.
        long appHwnd = 0, fgAfterForeground = 0, fgAtInject = 0;
        try { appHwnd = _session.RootElement.Properties.NativeWindowHandle.ValueOrDefault; }
        catch { }
        FocusThenInject(el, () => SendPhysicalInput(() =>
        {
            fgAfterForeground = (long)Win32.GetForegroundWindow();
            // sleep-ok (testing.md § convention 14): this is not a settle-wait — it
            // is the critical section being held open on purpose, and it is what
            // makes the caller's interleaving deterministic instead of timing-based.
            if (preDelayMs > 0) Thread.Sleep(preDelayMs);
            fgAtInject = (long)Win32.GetForegroundWindow();
            Keyboard.Type(text);
        }, $"diagnostic physical typing into {Describe(el)}"));
        return new PhysicalTypeReport(appHwnd, fgAfterForeground, fgAtInject);
    }

    /// <summary>
    /// Try appending <paramref name="text"/> to an element via ValuePattern.
    /// Returns false if ValuePattern isn't supported OR the SetValue call
    /// throws (e.g. AutoSuggestBox's ValuePattern fails with "Access is
    /// denied" because the composite peer can't write directly) so the
    /// caller can fall back to keyboard input.
    /// </summary>
    private static bool TrySetValueAppend(AutomationElement el, string text)
    {
        if (!el.Patterns.Value.IsSupported) return false;
        try
        {
            var current = el.Patterns.Value.Pattern.Value.Value ?? "";
            el.Patterns.Value.Pattern.SetValue(current + text);
            return true;
        }
        catch
        {
            return false;
        }
    }

    public void Select(string id, string value, int index = 0, IReadOnlyList<ScopeStep>? scope = null)
    {
        var el = Find(id, index, scope);
        var cf = _session.Automation.ConditionFactory;

        // ⚠ ORDER, and it is convention 11's own (tui settled it): the STRUCTURAL
        // check first — an element that is not a ComboBox is a wrong id, a different
        // bug class — then the actuation gate, and only then the option-membership
        // check below. A disabled picker's option list is routinely EMPTY for the
        // very reason it is disabled, so membership-first reports "'x' not found in
        // 'y' (visible items: )" for a control whose real story is that you cannot
        // touch it at all. Hoisting the not-a-ComboBox throw out of the else branch
        // is what makes that ordering expressible here.
        if (!el.Patterns.ExpandCollapse.IsSupported)
        {
            throw new InvalidOperationException(
                $"Element '{id}' does not support ExpandCollapse pattern (not a ComboBox)");
        }
        Gate("select", id, index, el);

        // ComboBox: expand, locate the ComboBoxItem whose Name/Content matches, click.
        el.Patterns.ExpandCollapse.Pattern.Expand();
        // ComboBox popup children attach to the ComboBox automation subtree in
        // WinUI 3; scan descendants for a ListItem whose Name equals value. A large
        // popup virtualizes — WinUI only realizes (and UIA can only see) the items
        // near the current scroll position, never the whole list — so a target far
        // down a "newest first" roster (e.g. the box claimer, the OLDEST account)
        // is invisible to a plain scan no matter how long it retries. The ComboBox
        // element ITSELF exposes the popup's Scroll pattern in WinUI 3 (verified
        // 2026-09-15: ListItem -> ComboBox(scroll) -> ...),
        // so once a plain scan comes up empty, page the popup down and rescan —
        // this is what a real user does, scroll then look, never a longer sleep.
        AutomationElement? match = null;
        var lastScrollPercent = double.MinValue;
        var scanDeadline = DateTime.UtcNow + TimeSpan.FromSeconds(2);
        for (var scrollStep = 0; scrollStep < 60 && match is null; scrollStep++)
        {
            while (DateTime.UtcNow < scanDeadline && match is null)
            {
                foreach (var candidate in el.FindAllDescendants(
                    cf.ByControlType(FlaUI.Core.Definitions.ControlType.ListItem)))
                {
                    string name;
                    try { name = candidate.Name ?? ""; } catch { name = ""; }
                    if (name == value)
                    {
                        match = candidate;
                        break;
                    }
                }
                if (match is null) Thread.Sleep(100);
            }
            if (match is not null) break;

            if (!el.Patterns.Scroll.IsSupported) break;
            var scroll = el.Patterns.Scroll.Pattern;
            if (scroll.VerticallyScrollable.ValueOrDefault != true) break;
            var pct = scroll.VerticalScrollPercent.ValueOrDefault;
            // No further progress (already at the bottom, or a percent read that
            // never moves) — one more scan pass just ran against this position, so
            // scanning again would only repeat it.
            if (pct >= 100.0 || Math.Abs(pct - lastScrollPercent) < 0.01) break;
            lastScrollPercent = pct;
            scroll.Scroll(FlaUI.Core.Definitions.ScrollAmount.NoAmount,
                FlaUI.Core.Definitions.ScrollAmount.LargeIncrement);
            Thread.Sleep(150); // let the newly-scrolled-in items realize
            scanDeadline = DateTime.UtcNow + TimeSpan.FromSeconds(2);
        }

        if (match is null)
        {
            // Snapshot the still-EXPANDED popup's currently-visible items before
            // collapsing — collapsing first (the prior order here) always emptied
            // the popup, so this message previously read "(visible items: )" on
            // EVERY failure regardless of what was actually painted.
            var visible = string.Join(", ", el.FindAllDescendants(
                cf.ByControlType(FlaUI.Core.Definitions.ControlType.ListItem))
                .Select(c => { try { return c.Name ?? ""; } catch { return "?"; } }));
            el.Patterns.ExpandCollapse.Pattern.Collapse();
            throw new InvalidOperationException(
                $"Select target '{value}' not found in '{id}' (visible items: {visible})");
        }

        if (match.Patterns.SelectionItem.IsSupported)
            match.Patterns.SelectionItem.Pattern.Select();
        else
            SendPhysicalInput(() => match.Click(), $"physical click on option '{value}' of {Describe(el)}");
    }

    public void Clear(string id, int index = 0, IReadOnlyList<ScopeStep>? scope = null)
    {
        var el = Find(id, index, scope);
        // WinUI CalendarDatePicker / TimePicker can't be cleared by text and are
        // disturbed by physical input; no-op (see IsNonTypeablePicker).
        if (IsNonTypeablePicker(el)) return;
        // Convention 11, after the structural check: emptying a disabled field is
        // the same illegal act as typing into one.
        Gate("clear", id, index, el);
        if (WatchForeground($"ValuePattern.SetValue(\"\") on {Describe(el)}",
                () => TrySetValue(el, "")))
            return;
        // AutoSuggestBox advertises ValuePattern on its outer peer but the
        // setter fails; the actual Edit child does accept SetValue. Reach
        // in via UIA so we never fall back to SendInput, which is blocked
        // by UIPI when FaunaApp is not the foreground window.
        var inner = FindInnerEditWithRetry(el, TimeSpan.FromSeconds(2));
        if (inner is not null && WatchForeground($"ValuePattern.SetValue(\"\") on {Describe(inner)}",
                () => TrySetValue(inner, "")))
            return;
        // Last resort: keyboard. SendInput requires FaunaApp foreground and can
        // throw Win32Exception(5) "Access is denied" when another process owns
        // it; SendPhysicalInput foregrounds + retries to clear that flake. No UIA
        // path beyond the ValuePattern already tried above (, judged).
        WithInputLock(() =>
        {
            TakeFocus(el, $"focus for physical clear of {Describe(el)}");
            PhysicalOnly($"clear {Describe(el)}", () =>
            {
                Keyboard.TypeSimultaneously(VirtualKeyShort.CONTROL, VirtualKeyShort.KEY_A);
                Keyboard.Press(VirtualKeyShort.DELETE);
            });
        });
    }

    /// <summary>
    /// WinUI CalendarDatePicker / DatePicker / TimePicker expose a
    /// ValuePattern-less composite peer that text can't set and physical typing
    /// disturbs. The unified create-event flow calls clear_and_type on
    /// event-dtstart / event-dtend (CalendarDatePickers); the app defaults a
    /// null picker to Now / Now+1h (mirroring macOS, whose AppleBridge typeText
    /// is likewise a no-op on its DatePicker), so Type/Clear safely no-op here.
    /// Keyed on the UIA ClassName so it covers any picker flavor.
    /// </summary>
    private static bool IsNonTypeablePicker(AutomationElement el)
    {
        string cls;
        try { cls = el.ClassName ?? ""; }
        catch { return false; }
        return cls.Contains("DatePicker", StringComparison.OrdinalIgnoreCase)
            || cls.Contains("TimePicker", StringComparison.OrdinalIgnoreCase);
    }

    private AutomationElement? FindInnerEdit(AutomationElement el)
    {
        try
        {
            var cf = _session.Automation.ConditionFactory;
            return el.FindFirstDescendant(
                cf.ByControlType(FlaUI.Core.Definitions.ControlType.Edit));
        }
        catch { return null; }
    }

    /// <summary>
    /// Find an AutoSuggestBox-style composite's inner <c>Edit</c> child, retrying
    /// for up to <paramref name="timeout"/>. The inner Edit's automation peer can
    /// lag the outer composite in the UIA tree right after a navigation — the
    /// composite realizes its template children on the first layout pass — so a
    /// single <see cref="FindInnerEdit"/> returns null and the caller (Type/Clear)
    /// would fall through to <see cref="SendPhysicalInput"/>. The inner Edit IS the
    /// UIA path (its ValuePattern.SetValue works where the outer composite peer
    /// throws "Access is denied"), so it's worth waiting for.
    ///
    /// <para><b>The wait is focus-free first.</b> UIA <c>SetFocus</c> is NOT
    /// focus-free: it activates the containing window (measured by
    /// <c>test_flaui_input_lock_windows.py</c>; see <see cref="FocusThenInject"/>), so
    /// a nudge here takes the keyboard focus from whatever the person at this desktop
    /// is typing into — e2e convention 10's windows focus axis. This comment used to
    /// say the opposite and the nudge fired on the FIRST miss, i.e. on every
    /// composite that realized a poll late. So the whole budget is polled without
    /// touching focus, and only a composite whose Edit never realized on its own gets
    /// one recorded <see cref="TakeFocus"/> and a short re-poll — still better than
    /// the physical fallback, which would take the foreground AND need an attached
    /// desktop. Returns the Edit, or null if it never materializes (the keyboard
    /// fallback then handles it).</para>
    /// </summary>
    private AutomationElement? FindInnerEditWithRetry(AutomationElement el, TimeSpan timeout)
    {
        var inner = PollInnerEdit(el, timeout);
        if (inner is not null) return inner;
        TakeFocus(el, $"focus nudge to realize the inner Edit of {Describe(el)}");
        return PollInnerEdit(el, TimeSpan.FromSeconds(1));
    }

    private AutomationElement? PollInnerEdit(AutomationElement el, TimeSpan timeout)
    {
        var deadline = DateTime.UtcNow + timeout;
        while (true)
        {
            var inner = FindInnerEdit(el);
            if (inner is not null) return inner;
            if (DateTime.UtcNow >= deadline) return null;
            Thread.Sleep(100);
        }
    }

    // ── Foreground-taking gestures (e2e convention 10, the windows focus axis) ──

    private static readonly object _foregroundTakesLock = new();
    private static readonly List<string> _foregroundTakes = new();

    /// <summary>
    /// Record that this bridge is about to move the desktop's foreground onto its
    /// app. Every such site in this file goes through here, so a run can say which
    /// gestures (and, through the harness's per-test drain, which tests) still take
    /// the keyboard focus from the person at this desktop. Process-wide, not per
    /// <see cref="Actions"/>: a relaunch mid-test builds a new instance, and the
    /// takes before it must not vanish with the old one.
    ///
    /// <para>The UIA gesture paths — Invoke, Toggle, SelectionItem, ExpandCollapse,
    /// ValuePattern, Scroll — reach here only through <see cref="WatchForeground"/>,
    /// when the app's own provider activated its window (a WinUI <c>TextBox</c>'s
    /// ValuePattern did, until the harness window became <c>WS_EX_NOACTIVATE</c>);
    /// that they never do is the property <c>test_windows_no_focus_steal.py</c> pins. What does reach here takes the
    /// foreground by necessity: physical <c>SendInput</c> needs it, and UIA
    /// <c>SetFocus</c> activates the window as a side effect.</para>
    /// </summary>
    private static void RecordForegroundTake(string gesture)
    {
        lock (_foregroundTakesLock) _foregroundTakes.Add(gesture);
        // `[bridge]` prefix: the one WindowsBridgeDriver._diagnostic_lines keeps.
        Console.Error.WriteLine($"[bridge] foreground-taking: {gesture}");
    }

    /// <summary>The foreground-taking gestures recorded since the last drain,
    /// oldest first; <paramref name="clear"/> empties the record.</summary>
    public static IReadOnlyList<string> ForegroundTakes(bool clear)
    {
        lock (_foregroundTakesLock)
        {
            var copy = _foregroundTakes.ToList();
            if (clear) _foregroundTakes.Clear();
            return copy;
        }
    }

    /// <summary>
    /// UIA <c>SetFocus</c> on <paramref name="el"/>, recorded as a foreground take
    /// and held under the input lock: it ACTIVATES the containing window, so an
    /// unlocked one steals the foreground from whichever bridge is mid-injection
    /// (see <see cref="FocusThenInject"/>). Best-effort — a focus failure is never a
    /// bridge 500 on these paths.
    /// </summary>
    private void TakeFocus(AutomationElement el, string why)
    {
        RecordForegroundTake(why);
        WithInputLock(() => { try { el.Focus(); } catch { } });
    }

    /// <summary>
    /// Run a UIA gesture and record it as a foreground take if the app did not own
    /// the foreground before it and does after. The record's other callers are
    /// gestures KNOWN to move the foreground; this catches the ones a UIA provider
    /// moves on its own — a pattern call is a COM call into the app, and what the
    /// app's provider does with it (a peer that focuses its control first) is the
    /// app's business, invisible from this side except by its effect. Measured,
    /// not assumed: that belief about <c>SetFocus</c> was wrong once already.
    /// </summary>
    private T WatchForeground<T>(string what, Func<T> gesture)
    {
        var before = AppOwnsForeground();
        var result = gesture();
        if (!before && AppOwnsForeground())
            RecordForegroundTake($"{what} (the app's UIA provider activated its window)");
        return result;
    }

    private void WatchForeground(string what, Action gesture)
        => WatchForeground(what, () => { gesture(); return true; });

    private bool AppOwnsForeground()
    {
        var appPid = _session.AppPid;
        if (appPid <= 0) return false;
        var fg = Win32.GetForegroundWindow();
        if (fg == IntPtr.Zero) return false;
        Win32.GetWindowThreadProcessId(fg, out var fgPid);
        return fgPid == (uint)appPid;
    }

    private static bool TrySetValue(AutomationElement el, string value)
    {
        if (!el.Patterns.Value.IsSupported) return false;
        try
        {
            el.Patterns.Value.Pattern.SetValue(value);
            return true;
        }
        catch { return false; }
    }

    public string GetText(string id, int index = 0, IReadOnlyList<ScopeStep>? scope = null)
    {
        // Retry for up to 2s — WinUI InfoBar needs render cycles after IsOpen=true
        // before its automation peer appears in the UIA tree.
        var el = FindWithRetry(id, index, TimeSpan.FromSeconds(2), scope);
        var cf = _session.Automation.ConditionFactory;
        return ElementFinder.GetText(el, cf);
    }

    public bool IsVisible(string id, IReadOnlyList<ScopeStep>? scope = null)
    {
        try
        {
            var root = _session.RootElement;
            var cf = _session.Automation.ConditionFactory;
            var scoped = ElementFinder.WalkScope(root, cf, scope);
            var elements = ElementFinder.FindAll(scoped, cf, id);
            return elements.Length > 0 && !elements[0].IsOffscreen;
        }
        // A crashed app is not an absent element (convention 11): let the
        // dead-peer report through instead of answering "not visible".
        catch (AppExitedException) { throw; }
        catch
        {
            return false;
        }
    }

    public bool IsEnabled(string id, int index, IReadOnlyList<ScopeStep>? scope = null)
    {
        try
        {
            var el = Find(id, index, scope);
            return el.IsEnabled;
        }
        // A crashed app is not an absent element (convention 11): let the
        // dead-peer report through instead of answering "not enabled".
        catch (AppExitedException) { throw; }
        catch
        {
            return false;
        }
    }

    /// <summary>
    /// Focus the element and press a single named key. Key names follow
    /// the web <c>KeyboardEvent.key</c> convention (<c>"Enter"</c>,
    /// <c>"Escape"</c>, <c>"Tab"</c>, …) and are mapped to FlaUI's
    /// <c>VirtualKeyShort</c>. Used for actions that commit input via a
    /// KeyDown handler instead of a confirm-button click — chip pickers,
    /// rename overlays.
    /// </summary>
    public void PressKey(string id, string key, int index = 0, IReadOnlyList<ScopeStep>? scope = null)
    {
        var el = Find(id, index, scope);
        var vk = key.ToLowerInvariant() switch
        {
            "enter" or "return" => VirtualKeyShort.ENTER,
            "escape" or "esc" => VirtualKeyShort.ESCAPE,
            "tab" => VirtualKeyShort.TAB,
            "backspace" => VirtualKeyShort.BACK,
            "delete" => VirtualKeyShort.DELETE,
            "arrowup" or "up" => VirtualKeyShort.UP,
            "arrowdown" or "down" => VirtualKeyShort.DOWN,
            "arrowleft" or "left" => VirtualKeyShort.LEFT,
            "arrowright" or "right" => VirtualKeyShort.RIGHT,
            "home" => VirtualKeyShort.HOME,
            "end" => VirtualKeyShort.END,
            _ => throw new InvalidOperationException(
                $"PressKey: unsupported key name '{key}'. Add a mapping in Actions.PressKey."),
        };
        // ONE critical section for the whole ceremony (see FocusThenInject): a
        // sibling that slipped in between the focus and the keypress would activate
        // ITS window and receive this key — which for an ESC is precisely the silent
        // ContentDialog cancel this lock exists to prevent. No UIA equivalent for a
        // bare keypress (, judged: no live caller of this
        // method exists to pin a specific conversion against).
        FocusThenInject(el, () =>
            // FlaUI's `Keyboard.Press(vk)` is keydown-only; without an explicit
            // release the key stays held and interferes with subsequent
            // actions. `Type(VirtualKeyShort[])` is the atomic press-and-release.
            PhysicalOnly($"press {key} on {Describe(el)}", () => Keyboard.Type(new[] { vk })),
            clickOnlyIfUnfocused: true);
        // Same post-key breathing room as Click: WinUI command chains
        // need a tick to flush before the next assertion runs.
        System.Threading.Thread.Sleep(150);
    }

    /// <summary>
    /// Read a named attribute on an element. Mapping:
    ///   * <c>"disabled"</c> — <c>"true"</c> / <c>"false"</c> based on <c>!IsEnabled</c>.
    ///   * <c>"name"</c> — <c>AutomationProperties.Name</c>.
    ///   * <c>"checked"</c> — the element's HelpText when it publishes one, else
    ///     <c>"true"</c> / <c>"false"</c> off its TogglePattern.
    ///   * <c>"options"</c> — a ComboBox's items as a JSON array of their Names,
    ///     in model order (<see cref="OptionsJson"/>); <c>null</c> on a non-ComboBox.
    ///   * any other key — <c>AutomationProperties.HelpText</c> (UIA exposes
    ///     a single application-supplied free-text channel per element).
    /// Returns <c>null</c> when the element isn't found or the property
    /// isn't set, so test-side <c>get_attr</c> can distinguish "absent"
    /// from "false".
    ///
    /// <para><paramref name="index"/> picks among same-id matches in document
    /// order, exactly as it does for <c>GetText</c>/<c>Click</c> — the contract
    /// <c>drivers/base.py</c> states for every driver. It was DROPPED here
    /// (hardcoded <c>Find(id, 0, scope)</c>, and <c>/element/attr</c> never even
    /// parsed the query parameter) until 2026-08-31, the identical bug the linux
    /// agent carried and fixed the same day. It is silent by construction: an
    /// indexed row's neighbour usually holds the same value, so row 0's answer
    /// is normally the right one — until a walk toggles row N and reads it
    /// back, which is precisely what the mail-import Scope step does.</para>
    /// </summary>
    public string? GetAttr(string id, string attr, int index = 0, IReadOnlyList<ScopeStep>? scope = null)
    {
        AutomationElement el;
        try
        {
            el = Find(id, index, scope);
        }
        catch
        {
            return null;
        }
        return AttrOf(el, attr);
    }

    /// <summary>
    /// <paramref name="attr"/> on EVERY element with <paramref name="id"/>, in
    /// the same order <c>index</c> addresses them in — one find for the whole
    /// column instead of one find per element.
    ///
    /// <para>This exists because <see cref="Find"/> is O(tree): it runs a whole
    /// <see cref="ElementFinder.FindAll"/> and then keeps <c>all[index]</c>, so a
    /// caller reading N same-id rows one at a time pays N walks to read N
    /// properties, each walk served by the app's UI thread. Measured against a
    /// 61-bubble conversations thread: ~1.25 s per <c>/element/attr</c> call,
    /// ~76 s for one pass over the thread, and a test spending ~96 % of its wall
    /// time in two such loops. One walk, N property
    /// reads collapses that pass to a single round trip.</para>
    ///
    /// <para>The per-element answer comes from the SAME <see cref="AttrOf"/> the
    /// single-element route uses, so the two can never drift into different
    /// answers for one attribute — the bulk route is a batching of
    /// <see cref="GetAttr"/>, not a second implementation of it.</para>
    ///
    /// <para>An id with no matches answers an EMPTY ARRAY, never a 404: zero
    /// rows is a real answer to "read the column", and the driver relies on that
    /// — it treats a 404 from this route as "this bridge is too old to serve
    /// it" and silently falls back to the slow per-element loop
    /// (<c>drivers/http_bridge.py::_bulk_read</c>).</para>
    /// </summary>
    public string?[] GetAttrs(string id, string attr, IReadOnlyList<ScopeStep>? scope = null)
    {
        var elements = FindAllFor(id, scope);
        var values = new string?[elements.Length];
        for (var i = 0; i < elements.Length; i++)
        {
            // Per-element failure is per-element: one row whose property read
            // throws must not lose the other 60. null is already this
            // contract's "not set / doesn't apply".
            try { values[i] = AttrOf(elements[i], attr); }
            catch { values[i] = null; }
        }
        return values;
    }

    /// <summary>
    /// The text of EVERY element with <paramref name="id"/>, in <c>index</c>
    /// order — the bulk twin of <see cref="GetText"/>; see
    /// <see cref="GetAttrs"/> for why the route exists.
    ///
    /// <para>Deliberately NOT <see cref="FindWithRetry"/>: that 2 s retry is for
    /// a single element whose UIA peer may not have been realized yet (the
    /// InfoBar case), and a bulk read over a list that is legitimately empty
    /// would burn the retry on every call. A caller that needs the rows to
    /// exist waits for them the way it always did, then reads the column.</para>
    /// </summary>
    public string[] GetTexts(string id, IReadOnlyList<ScopeStep>? scope = null)
    {
        var cf = _session.Automation.ConditionFactory;
        var elements = FindAllFor(id, scope);
        var texts = new string[elements.Length];
        for (var i = 0; i < elements.Length; i++)
        {
            try { texts[i] = ElementFinder.GetText(elements[i], cf); }
            catch { texts[i] = ""; }
        }
        return texts;
    }

    /// <summary>The one scope walk + one <see cref="ElementFinder.FindAll"/>
    /// both bulk reads share. Mirrors <see cref="Count"/>'s shape exactly —
    /// including letting <see cref="AppExitedException"/> through, because a
    /// crashed app is not an empty column (convention 11).</summary>
    private AutomationElement[] FindAllFor(string id, IReadOnlyList<ScopeStep>? scope)
    {
        try
        {
            var root = _session.RootElement;
            var cf = _session.Automation.ConditionFactory;
            var scoped = ElementFinder.WalkScope(root, cf, scope);
            return ElementFinder.FindAll(scoped, cf, id);
        }
        catch (AppExitedException) { throw; }
        catch
        {
            return [];
        }
    }

    /// <summary>The per-element half of <see cref="GetAttr"/>, shared with
    /// <see cref="GetAttrs"/> so the single and bulk routes can never answer
    /// one attribute differently. Mapping is documented on
    /// <see cref="GetAttr"/>.</summary>
    private string? AttrOf(AutomationElement el, string attr)
    {
        switch (attr.ToLowerInvariant())
        {
            case "frame":
                // The cross-driver geometry contract `drivers/base.py`
                // documents for `assert_on_screen`: window-space "x,y,w,h".
                // Until 2026-09-11 windows had no case here, so a frame read
                // fell through to HelpText and answered whatever OTHER
                // attribute that element happened to publish — a WRONG answer,
                // which is worse than an unimplemented one, and it left
                // "present but IsOffscreen" (count>=1, visible=False) an
                // unanswerable question on the one platform whose is_visible
                // is *defined* as !IsOffscreen. Screen-space rects are
                // rebased on the top-level window so a negative origin means
                // what the contract says it means: pushed outside the window.
                {
                    try
                    {
                        var r = el.BoundingRectangle;
                        double ox = 0, oy = 0;
                        var cur = el;
                        for (var i = 0; i < 40 && cur is not null; i++)
                        {
                            if (cur.ControlType == FlaUI.Core.Definitions.ControlType.Window)
                            {
                                var w = cur.BoundingRectangle;
                                ox = w.X; oy = w.Y;
                                break;
                            }
                            try { cur = cur.Parent; } catch { break; }
                        }
                        return $"{r.X - ox},{r.Y - oy},{r.Width},{r.Height}";
                    }
                    catch { return null; }
                }
            case "in-viewport":
                // The cross-driver "brought into view" observable
                // (`drivers/base.py::in_viewport`, `conversations.md` § The
                // selected message), answered on windows the same way linux and
                // tui answer it: TRUE when the element's vertical CENTRE lies
                // inside its nearest vertically-scrollable ancestor's visible
                // band. Distinct from `visible`/IsOffscreen, which stays true
                // for a row scrolled far out of view — WinUI realizes rows the
                // viewport does not show, so the registry alone cannot say.
                //
                // It MUST be an explicit case: the `default` arm below answers
                // every unknown attribute with HelpText, which this app already
                // uses as the `selected` channel — so a missing case here would
                // not fail, it would quietly answer the WRONG element state.
                //
                // Centre-of-element, not area-overlap: `VisibleFraction` exists
                // for the "substantially visible" question (`ScrollIntoViewFraction`),
                // and a half-clipped row at the edge of the band is genuinely
                // "in view" for this contract while its area fraction is not.
                // No scrolling — a read, never a gesture.
                {
                    try
                    {
                        var viewport = FindScrollableAncestor(el);
                        // No scroller above it: nothing can have scrolled it out
                        // of view, so UIA's own offscreen bit is the whole answer.
                        if (viewport is null) return el.IsOffscreen ? "false" : "true";
                        var r = el.BoundingRectangle;
                        var vp = viewport.BoundingRectangle;
                        if (r.Height <= 0 || vp.Height <= 0) return "false";
                        var centre = r.Y + (r.Height / 2.0);
                        var inBand = centre >= vp.Y && centre <= vp.Y + vp.Height;
                        return inBand && !el.IsOffscreen ? "true" : "false";
                    }
                    catch { return null; }
                }
            case "disabled":
                // Walk up the parent chain — UIA's IsEnabled doesn't
                // propagate from a host UserControl to inner non-Control
                // elements (Border, StackPanel) at the UIA layer, even
                // though XAML's visual IsEnabled propagation does. If
                // ANY ancestor in this app's window is disabled, treat
                // this element as disabled. Stop at the main window root.
                {
                    var cur = el;
                    var rootHwnd = (IntPtr)_session.RootElement.Properties.NativeWindowHandle.ValueOrDefault;
                    while (cur is not null)
                    {
                        if (!cur.IsEnabled) return "true";
                        if ((IntPtr)cur.Properties.NativeWindowHandle.ValueOrDefault == rootHwnd) break;
                        try { cur = cur.Parent; }
                        catch { break; }
                        if (cur is null) break;
                    }
                    return "false";
                }
            case "checked":
                // The cross-driver checkbox/toggle contract (`drivers/base.py::get_attr`:
                // "true" / "false"). An element that publishes its own answer on HelpText
                // keeps it — the room roster's toggles do, because their HelpText is the
                // COMMITTED state while the toggle shows the staged one. Anything else
                // answers off its TogglePattern, which IS the control's live state, so a
                // plain CheckBox needs no mirrored HelpText to be readable.
                {
                    try
                    {
                        var help = el.Properties.HelpText.ValueOrDefault;
                        if (!string.IsNullOrEmpty(help)) return help;
                        if (!el.Patterns.Toggle.IsSupported) return null;
                        return el.Patterns.Toggle.Pattern.ToggleState.Value switch
                        {
                            FlaUI.Core.Definitions.ToggleState.On => "true",
                            FlaUI.Core.Definitions.ToggleState.Off => "false",
                            _ => null,
                        };
                    }
                    catch { return null; }
                }
            case "options":
                // The cross-driver picker contract (`drivers/base.py::option_texts`):
                // a JSON array of every option the picker offers, in model order, or
                // null for a non-picker — never "[]", which would claim "a picker
                // with zero options". An explicit case for the same reason as
                // `in-viewport`: the `default` arm would answer HelpText instead.
                //
                // Each option reads as its ListItem's Name — the exact membership set
                // `Select` matches against, so "offered" and "selectable" can never
                // disagree on this bridge (a picker whose items carry a stable-key
                // AutomationProperties.Name reports those keys; the action layer
                // normalizes them onto the same keys as the painted labels).
                try { return el.ControlType == FlaUI.Core.Definitions.ControlType.ComboBox ? OptionsJson(el) : null; }
                catch (AppExitedException) { throw; }
                catch { return null; }
            case "name":
                try { return el.Properties.Name.ValueOrDefault; }
                catch { return null; }
            default:
                try { return el.Properties.HelpText.ValueOrDefault; }
                catch { return null; }
        }
    }

    public int Count(string id, IReadOnlyList<ScopeStep>? scope = null)
    {
        try
        {
            var root = _session.RootElement;
            var cf = _session.Automation.ConditionFactory;
            var scoped = ElementFinder.WalkScope(root, cf, scope);
            return ElementFinder.FindAll(scoped, cf, id).Length;
        }
        // A crashed app is not an absent element (convention 11): let the
        // dead-peer report through instead of answering "count 0".
        catch (AppExitedException) { throw; }
        catch
        {
            return 0;
        }
    }

    /// <summary>The <c>AutomationProperties.ItemStatus</c> value
    /// <c>ControlGate</c> (<c>apps/fauna-windows/FaunaApp/FaunaApp/Helpers/
    /// OfflineGateExtensions.cs</c>) stamps on every gate-declared control —
    /// duplicated here as a literal because the bridge is a standalone process
    /// with no reference to <c>FaunaApp.Core</c>/<c>FaunaApp</c>; the two sides
    /// stay in sync by this cross-reference, not a shared assembly.</summary>
    private const string GateDeclaredMarker = "fauna-gate-declared";

    /// <summary>
    /// <c>GET /registry</c> — every element the app currently publishes, as
    /// records (<c>drivers/base.py::registry_snapshot</c>'s cross-app contract;
    /// e2e-conventions.md convention 17 / <c>e2e-systematic-ui-walks.md</c>'s
    /// whole-frame half). Mirrors <c>web-bridge/server.py</c>'s
    /// <c>_REGISTRY_SNAPSHOT_JS</c> field-for-field.
    ///
    /// <para><b>Two passes, same shape as the JS reference.</b> Pass 1 walks
    /// <c>root.FindAllDescendants()</c> — the SAME unfiltered traversal
    /// <c>ElementFinder.FindAll</c>'s <c>FindAllDescendants(cf.ByAutomationId(id))</c>
    /// filters from, so the per-id occurrence <c>index</c> computed here is
    /// EXACTLY what an index-addressed action call resolves against (the
    /// "re-driven verbatim" contract) — and keeps only VISIBLE
    /// (<c>!IsOffscreen</c>), id-bearing elements, matching <see cref="IsVisible"/>'s
    /// own check. Pass 2 walks each kept element's ancestor chain, emitting a
    /// step for every ancestor that ALSO made the kept set — the same
    /// "id[index]/id[index]" scope DSL <c>_scoped_root</c> / web's
    /// <c>scopeOf</c> use, stopping at the main window root.</para>
    ///
    /// <para><b><c>declares_enabled</c> answers a real UIA question, honestly</b>
    /// (the row's own warning: a fabricated field is worse than no route). UIA's
    /// raw <c>IsEnabled</c> cannot distinguish a control a gate declared from one
    /// that merely defaulted to enabled — so this reads the
    /// <see cref="GateDeclaredMarker"/> stamp <c>ControlGate</c> sets at
    /// declare-time instead of guessing from control type or state, the same
    /// answer web gives by reading its own declare-time
    /// <c>data-offline-gate-declared</c> attribute rather than inferring from
    /// the DOM.</para>
    /// </summary>
    public List<Dictionary<string, object?>> RegistrySnapshot()
    {
        var root = _session.RootElement;
        var cf = _session.Automation.ConditionFactory;
        IntPtr rootHwnd;
        try { rootHwnd = (IntPtr)root.Properties.NativeWindowHandle.ValueOrDefault; }
        catch { rootHwnd = IntPtr.Zero; }

        // Below NavigationView's ~641 DIP "Minimal" pane-display threshold — which
        // App.xaml.cs's 600 DIP minimum window size sits inside — the pane is an
        // unopened flyout and its NavigationViewItems (every "-tab" id) are absent
        // from the tree entirely, not merely offscreen: a whole-frame scan silently
        // dropped every nav tab. `ElementFinder`'s by-id lookup already opens the
        // pane on a miss for exactly this reason (`ToggleNavPane`, its
        // "s3-toggle-pane-retry" strategy) — probe the one nav item every
        // authenticated page carries (`feed-tab`) and open-scan-restore around it,
        // so the registry doesn't quietly omit every nav item when the pane starts
        // closed. A pre-login frame has no `TogglePaneButton` either, so the probe
        // miss there is a harmless no-op (`ToggleNavPane` returns immediately).
        bool openedPane = false;
        if (root.FindFirstDescendant(cf.ByAutomationId("feed-tab")) is null)
        {
            ElementFinder.ToggleNavPane(root, cf);
            openedPane = true;
        }
        try
        {
            var all = root.FindAllDescendants();

            var indexCounters = new Dictionary<string, int>();
            var kept = new List<(AutomationElement El, string Id, int Index, string Key)>();
            foreach (var el in all)
            {
                string autoId;
                try { autoId = el.AutomationId ?? ""; } catch { continue; }
                if (string.IsNullOrEmpty(autoId)) continue;
                bool offscreen;
                try { offscreen = el.IsOffscreen; } catch { continue; }
                if (offscreen) continue;

                var idx = indexCounters.TryGetValue(autoId, out var n) ? n : 0;
                indexCounters[autoId] = idx + 1;
                kept.Add((el, autoId, idx, RuntimeKey(el)));
            }

            var byKey = new Dictionary<string, (string Id, int Index)>();
            foreach (var e in kept) byKey[e.Key] = (e.Id, e.Index);

            var records = new List<Dictionary<string, object?>>();
            foreach (var (el, autoId, idx, _) in kept)
            {
                bool enabled;
                try { enabled = el.IsEnabled; } catch { enabled = false; }

                bool actuable = false;
                try
                {
                    actuable = el.Patterns.Invoke.IsSupported
                        || el.Patterns.Toggle.IsSupported
                        || el.Patterns.SelectionItem.IsSupported
                        || el.Patterns.ExpandCollapse.IsSupported;
                }
                catch { }

                bool editable = false;
                try
                {
                    editable = el.Patterns.Value.IsSupported
                        && !el.Patterns.Value.Pattern.IsReadOnly.ValueOrDefault;
                }
                catch { }

                bool declaresEnabled = false;
                try { declaresEnabled = el.Properties.ItemStatus.ValueOrDefault == GateDeclaredMarker; }
                catch { }

                records.Add(new Dictionary<string, object?>
                {
                    ["id"] = autoId,
                    ["index"] = idx,
                    ["enabled"] = enabled,
                    ["declares_enabled"] = declaresEnabled,
                    ["actuable"] = actuable,
                    ["editable"] = editable,
                    ["scope"] = ScopeOf(el, byKey, rootHwnd),
                });
            }
            return records;
        }
        finally
        {
            if (openedPane)
                ElementFinder.ToggleNavPane(root, cf);
        }
    }

    /// <summary>The "id[index]/id[index]" ancestor-scope DSL for <paramref name="el"/>
    /// — every ancestor between it and the main window root that is ITSELF a kept
    /// (visible, id-bearing) registry row, outermost first. Mirrors web's
    /// <c>scopeOf</c>: only an ancestor a caller could actually address as a scope
    /// step qualifies.</summary>
    private string ScopeOf(AutomationElement el, Dictionary<string, (string Id, int Index)> byKey, IntPtr rootHwnd)
    {
        var steps = new List<string>();
        AutomationElement? cur;
        try { cur = el.Parent; } catch { return ""; }
        for (int i = 0; cur is not null && i < 64; i++)
        {
            try
            {
                if (rootHwnd != IntPtr.Zero &&
                    (IntPtr)cur.Properties.NativeWindowHandle.ValueOrDefault == rootHwnd)
                    break;
            }
            catch { }
            if (byKey.TryGetValue(RuntimeKey(cur), out var entry))
                steps.Insert(0, $"{entry.Id}[{entry.Index}]");
            try { cur = cur.Parent; } catch { break; }
        }
        return string.Join("/", steps);
    }

    /// <summary>UIA-identity key for dictionary lookups — <c>RuntimeId</c>, the
    /// same canonical per-session identity <see cref="ElementFinder"/>'s own
    /// <c>SameElement</c> compares by, joined into a string.</summary>
    private static string RuntimeKey(AutomationElement el)
    {
        try
        {
            var rid = el.Properties.RuntimeId.ValueOrDefault;
            if (rid is not null) return string.Join(",", rid);
        }
        catch { }
        try { return "hwnd:" + el.Properties.NativeWindowHandle.ValueOrDefault; }
        catch { return Guid.NewGuid().ToString(); }
    }

    /// <summary>
    /// Scroll the page via the UIA ScrollPattern on the WIDEST scrollable
    /// container, falling back to a PageDown/PageUp keypress. The keypress path
    /// alone is unreliable for reaching deep elements in a long ScrollViewer: it
    /// needs keyboard focus inside the scroll region (often not set after a nav)
    /// and doesn't foreground the app. ScrollPattern.Scroll moves the viewport
    /// directly with no focus dependency, so offscreen elements (e.g. the
    /// settings quota-section, the revealed email-filter form) come into view and
    /// IsVisible (which checks !IsOffscreen) can succeed. Returns true if a
    /// ScrollPattern container was actuated.
    ///
    /// <para>Picking the WIDEST candidate (by bounding-rect area — see
    /// <see cref="ScrollPolicy.WidestIndex"/>), not the first one the UIA tree
    /// enumerates, matters because a settings sub-page's 24-item navigation rail
    /// (<c>SettingsShellPage.xaml</c>'s <c>NavigationView.MenuItems</c>) precedes
    /// the actual page content in tree order. Picking first meant every one of
    /// `wait_for`'s post-sweep blind-scroll retries on a settings page was
    /// silently scrolling the rail, never the content it was
    /// asked for. This has no target element to
    /// prefer a scrollable ANCESTOR of (that is what
    /// <see cref="ScrollIntoView"/>/<see cref="FindScrollableAncestor"/> are for) —
    /// it is the no-target, whole-page fallback, so "biggest region on screen" is
    /// the best available proxy for "the content area", not tree position.</para>
    /// </summary>
    public bool Scroll(string direction)
    {
        var amount = direction == "up"
            ? FlaUI.Core.Definitions.ScrollAmount.LargeDecrement
            : FlaUI.Core.Definitions.ScrollAmount.LargeIncrement;

        // Collect every descendant that advertises a vertical ScrollPattern, then
        // pick the widest by bounding-rect area (ScrollPolicy.WidestIndex) — never
        // just the first one the UIA tree enumerates.
        var root = _session.RootElement;
        var candidates = new List<AutomationElement>();
        var areas = new List<double>();
        try
        {
            foreach (var el in root.FindAllDescendants())
            {
                if (el.Patterns.Scroll.IsSupported &&
                    el.Patterns.Scroll.Pattern.VerticallyScrollable.ValueOrDefault)
                {
                    double area;
                    try
                    {
                        var r = el.BoundingRectangle;
                        area = r.Width * r.Height;
                    }
                    catch { area = 0; }
                    candidates.Add(el);
                    areas.Add(area);
                }
            }
        }
        catch { }

        var widest = ScrollPolicy.WidestIndex(areas);
        var scrollable = widest >= 0 ? candidates[widest] : null;

        if (scrollable is not null)
        {
            try
            {
                scrollable.Patterns.Scroll.Pattern.Scroll(
                    FlaUI.Core.Definitions.ScrollAmount.NoAmount, amount);
                return true;
            }
            catch { }
        }

        // Fallback: foreground + PageDown/PageUp keypress — no ScrollPattern
        // descendant found, so there is no UIA scroll to prefer
        // (, judged).
        var key = direction == "up"
            ? FlaUI.Core.WindowsAPI.VirtualKeyShort.PRIOR
            : FlaUI.Core.WindowsAPI.VirtualKeyShort.NEXT;
        PhysicalOnly($"scroll {direction} (no ScrollPattern descendant)",
            () => Keyboard.Type(new[] { key }));
        return false;
    }

    /// <summary>
    /// Post a real WM_CLOSE to the app's main window HWND — the OS message a
    /// titlebar X click sends. WinUI 3's <c>AppWindow.Closing</c> is WM_CLOSE-driven
    /// (see <c>TrayIconService.Initialize</c>'s own comment on the fact), so this
    /// exercises the SAME close-request path a physical click does: close-to-tray's
    /// hide-vs-quit decision, and — when it decides to quit —
    /// <c>TrayIconService.QuitApplication()</c>'s real flush-then-exit sequence, both
    /// run for real. <c>SessionManager.Quit()</c> (session teardown/relaunch)
    /// deliberately does NOT go through here — it force-<c>Kill()</c>s the process,
    /// which is what a verified-dead relaunch needs and neither flushes state nor
    /// honours close-to-tray.
    ///
    /// Fire-and-forget: a quit-path close can exit the app process (and race this
    /// HTTP reply) before this call would otherwise return. The caller observes the
    /// outcome via the session's process handle, never this method's return — see
    /// the python driver's <c>is_app_alive()</c> / <c>wait_app_exit()</c>.
    /// </summary>
    public void WindowClose()
    {
        IntPtr hwnd;
        try
        {
            hwnd = (IntPtr)_session.RootElement.Properties.NativeWindowHandle.ValueOrDefault;
        }
        catch
        {
            return; // No window to close — nothing to do.
        }
        if (hwnd == IntPtr.Zero) return;
        Win32.PostMessageW(hwnd, Win32.WM_CLOSE, IntPtr.Zero, IntPtr.Zero);
    }

    /// <summary>
    /// Bring a single element into the viewport of its nearest scrollable
    /// ancestor. The blind page-step Scroll(direction) above overshoots
    /// mid-page elements: a LargeIncrement can move an offscreen-below element
    /// straight to offscreen-above without ever landing it visible, so the
    /// is-visible poll between page steps never catches it (the linked-nests
    /// add button — above the reachable quota section — was the canonical
    /// victim). This targets the element directly:
    ///   1. ScrollItemPattern.ScrollIntoView() when supported (items controls);
    ///   2. else a SetScrollPercent sweep of the ancestor ScrollViewer, one
    ///      viewport per step (<see cref="ScrollPolicy.SweepStepViewportFraction"/>
    ///      — see that constant's own doc comment for why a FULL viewport per step
    ///      is gap-free, not merely a fixed fraction reaching for the same thing).
    /// Returns true once the element reports IsOffscreen == false.
    ///
    /// <para><paramref name="index"/> picks among same-id matches exactly as every
    /// other element route does (the driver's <c>scroll_to(element_id, index)</c>
    /// contract). Until 2026-09-25 this route dropped it and always scrolled
    /// <c>elements[0]</c>, so an indexed scroll brought a DIFFERENT row into view
    /// and reported success — measured on the events day grid, where scrolling
    /// the 10:00 block (index 2) landed the view on the 23:00 one (index 0).</para>
    ///
    /// <para>Measured cost of getting the step policy wrong: <c>POST
    /// /element/scroll-into-view</c> held the bridge's UIA gate for 17 s, 25 s and
    /// 21 s, three times per run, in five consecutive runs of
    /// `test_bundled_provider.py --app windows` — about 63 s of a 90 s budget spent
    /// scrolling, with UIA COM calls timing out around
    /// it.</para>
    ///
    /// <para>WHERE that cost actually is, measured by the per-call split this method
    /// logs: <c>SetScrollPercent</c> costs ~2.01 s EVERY call (14109 ms over 7),
    /// while the <c>IsOffscreen</c> and scroll-percent READS in the same steps cost
    /// 1 ms and 0 ms. So a step is one expensive UI-thread-marshalled pattern call
    /// plus two nearly-free property reads — "every UIA call against this app costs
    /// ~2 s" is refuted, and the step COUNT, not the read count, is the whole
    /// budget. Do not add steps to buy precision; buy
    /// it from the reads.</para>
    /// </summary>
    public bool ScrollIntoView(string id, int index = 0, IReadOnlyList<ScopeStep>? scope = null)
    {
        var total = System.Diagnostics.Stopwatch.StartNew();
        var phase = System.Diagnostics.Stopwatch.StartNew();
        var steps = 0;
        var find = 0L; var scrollItem = 0L; var ancestor = 0L; var sweep = 0L;
        // Per-UIA-call costs inside the sweep. The phase totals above say a sweep
        // took 14 s; only these say WHICH of the three cross-process calls a step
        // makes spent it. Every sweep this bridge has ever logged — four different
        // pages, found and not-found alike — cost ~2.07 s per step, which is the
        // shape of one fixed timeout, not of variable UI-thread load; splitting the
        // step is what tells those two apart.
        var setMs = 0L; var offMs = 0L; var readMs = 0L;
        var geometry = "";
        var outcome = "not-found";
        try
        {
            var root = _session.RootElement;
            var cf = _session.Automation.ConditionFactory;
            var scoped = ElementFinder.WalkScope(root, cf, scope);
            var elements = ElementFinder.FindAll(scoped, cf, id);
            if (elements.Length <= index) return false;
            var el = elements[index];
            if (!el.IsOffscreen) { outcome = "already-visible"; return true; }
            find = phase.ElapsedMilliseconds; phase.Restart();

            // 1. Items-control rows expose ScrollItemPattern — exact and cheap.
            try
            {
                if (el.Patterns.ScrollItem.IsSupported)
                {
                    el.Patterns.ScrollItem.Pattern.ScrollIntoView();
                    if (!el.IsOffscreen) { outcome = "scroll-item"; return true; }
                }
            }
            catch { }
            scrollItem = phase.ElapsedMilliseconds; phase.Restart();

            // 2. Sweep the nearest vertically-scrollable ancestor.
            var container = FindScrollableAncestor(el);
            if (container is null) { outcome = "no-scrollable-ancestor"; return !el.IsOffscreen; }
            var sp = container.Patterns.Scroll.Pattern;
            if (!sp.VerticallyScrollable.ValueOrDefault) { outcome = "not-scrollable"; return !el.IsOffscreen; }
            ancestor = phase.ElapsedMilliseconds; phase.Restart();

            // The viewport as a percentage of the scrollable content — the number
            // that decides the sweep's step plan (ScrollPolicy.ComputeSweepTargets).
            // Read once (it does not change mid-sweep); an unreadable or nonsensical
            // value falls back to the policy's own fixed floor, never to a larger step.
            double viewSize;
            try { viewSize = sp.VerticalViewSize.ValueOrDefault; }
            catch { viewSize = 0; }

            var lastPercent = double.NaN;
            var call = new System.Diagnostics.Stopwatch();
            // Targets always end at exactly 100 regardless of whether the step divides
            // it evenly — the loop used to stop at the largest MULTIPLE of step below
            // 100, which left a page's final slice permanently unreachable whenever the
            // viewport's own size didn't divide 100 evenly.
            var sweepTargets = ScrollPolicy.ComputeSweepTargets(
                viewSize, ScrollPolicy.SweepStepViewportFraction, ScrollPolicy.SweepStepMin, ScrollPolicy.SweepStepMax);
            foreach (var target in sweepTargets)
            {
                call.Restart();
                try { sp.SetScrollPercent(-1, target); } // -1 = leave horizontal as-is
                catch { setMs += call.ElapsedMilliseconds; break; }
                setMs += call.ElapsedMilliseconds;
                steps++;
                Thread.Sleep(40);
                call.Restart();
                var onscreen = !el.IsOffscreen;
                offMs += call.ElapsedMilliseconds;
                if (onscreen) { outcome = "swept"; return true; }

                // The container clamps at its own maximum, so once the achieved
                // position stops advancing the remaining requests are no-ops that
                // still cost a full round trip each. Reading it back is one extra
                // call per step and saves every step after the bottom.
                double achieved;
                call.Restart();
                try { achieved = sp.VerticalScrollPercent.ValueOrDefault; }
                catch { readMs += call.ElapsedMilliseconds; break; }
                readMs += call.ElapsedMilliseconds;
                if (!double.IsNaN(lastPercent) && Math.Abs(achieved - lastPercent) < 0.5)
                {
                    outcome = "bottom-reached";
                    break;
                }
                lastPercent = achieved;
            }
            sweep = phase.ElapsedMilliseconds;
            // A sweep that ran to its bound without the element ever reporting
            // onscreen is the one outcome the per-call costs above cannot explain
            // on their own: it says the container was driven to its maximum and
            // the element still calls itself invisible. Name the geometry so the
            // next reader can tell "the scroll never reached it" from "the scroll
            // reached it and the window clips it" without another run.
            if (outcome == "not-found") geometry = DescribeGeometry(el, container, viewSize, lastPercent);
            if (outcome == "not-found") outcome = "swept-without-finding";
            return !el.IsOffscreen;
        }
        catch (Exception e)
        {
            // NAME the exception. `scroll_to` is best-effort by contract — a caller
            // that cannot scroll still gets its plain visibility answer — so this
            // swallow is correct, but a swallow that says only "threw" is how a
            // 2 s UIA provider timeout (the app's UI thread not pumping) reads as
            // "the element is not scrollable", which is a different bug with a
            // different owner. One line separates them.
            outcome = $"threw {e.GetType().Name}: {e.Message.Split('\n')[0]}";
            return false;
        }
        finally
        {
            if (phase.IsRunning && sweep == 0) sweep = phase.ElapsedMilliseconds;
            T($"scroll-into-view {id}: {outcome} in {total.ElapsedMilliseconds}ms "
              + $"(find={find}ms scroll-item={scrollItem}ms ancestor={ancestor}ms "
              + $"sweep={sweep}ms over {steps} step(s)"
              + (steps > 0 ? $"; per-call set={setMs}ms is-offscreen={offMs}ms "
                             + $"read-back={readMs}ms" : "")
              + ")"
              + (geometry.Length > 0 ? $" {geometry}" : ""));
        }
    }

    /// <summary>
    /// The three rectangles that decide whether a swept-to element could have been
    /// seen, plus the scroll numbers that were supposed to put it there.
    ///
    /// <para>An element reports <c>IsOffscreen</c> when its rect is empty OR clipped
    /// away by an ancestor — including the top-level window. So a container scrolled
    /// to its own maximum can still leave its last children unseen if the container's
    /// viewport itself extends past the window's client area: the scroll is at 100%,
    /// the element has a real rect, and it is invisible anyway. That case and "the
    /// element has a zero-height rect" and "the container never actually moved" are
    /// indistinguishable from the outcome word alone, and each has a different owner
    /// — a window-clipped container is an APP layout bug a human hits too, not a
    /// bridge bug. Read these three rects before choosing.</para>
    /// </summary>
    private static string DescribeGeometry(
        AutomationElement el, AutomationElement container, double viewSize, double lastPercent)
    {
        static string Rect(AutomationElement? e)
        {
            if (e is null) return "?";
            try
            {
                var r = e.BoundingRectangle;
                return $"({r.X},{r.Y} {r.Width}x{r.Height})";
            }
            catch (Exception ex) { return $"<{ex.GetType().Name}>"; }
        }

        // The top-level window, walked from the element rather than taken from the
        // session: the session's root is re-resolved through ElementFromHandle, which
        // is itself the call that times out when the app stops pumping.
        AutomationElement? window = null;
        try
        {
            var cur = el;
            for (var i = 0; i < 40 && cur is not null; i++)
            {
                if (cur.ControlType == FlaUI.Core.Definitions.ControlType.Window) { window = cur; break; }
                cur = cur.Parent;
            }
        }
        catch { }

        return $"[geometry el={Rect(el)} container={Rect(container)} window={Rect(window)} "
               + $"view-size={viewSize:0.#}% achieved={(double.IsNaN(lastPercent) ? "?" : lastPercent.ToString("0.#"))}%]";
    }

    /// <summary>If <paramref name="el"/> is offscreen, call ScrollItemPattern on it
    /// or on its nearest ancestor that supports it (the items-control row that
    /// hosts it). Best-effort: the physical click that follows raises the real
    /// error if the element still has no clickable point.</summary>
    private static void BringRowIntoView(AutomationElement el)
    {
        try
        {
            if (!el.IsOffscreen) return;
            // Keep climbing until a call actually lands. A WinUI TextBlock inside a
            // row ADVERTISES ScrollItem, but its ScrollIntoView is a no-op (traced
            // live: the element stayed offscreen). The hosting ListViewItem's call
            // is the one that scrolls.
            var cur = el;
            for (var i = 0; i < 6 && cur is not null; i++)
            {
                if (cur.Patterns.ScrollItem.IsSupported)
                {
                    cur.Patterns.ScrollItem.Pattern.ScrollIntoView();
                    if (!el.IsOffscreen)
                    {
                        T($"bring-row-into-view {Describe(el)}: scroll-item on ancestor {i} ({cur.ControlType})");
                        return;
                    }
                }
                cur = cur.Parent;
            }
            // Traced live on the Events calendar list: no ScrollItem call there moves
            // anything. Sweep the nearest scrollable ancestor with the same plan
            // ScrollIntoView uses.
            var container = FindScrollableAncestor(el);
            if (container is null)
            {
                T($"bring-row-into-view {Describe(el)}: no ScrollItem call landed, no scrollable ancestor");
                return;
            }
            var sp = container.Patterns.Scroll.Pattern;
            double viewSize;
            try { viewSize = sp.VerticalViewSize.ValueOrDefault; }
            catch { viewSize = 0; }
            foreach (var target in ScrollPolicy.ComputeSweepTargets(
                viewSize, ScrollPolicy.SweepStepViewportFraction, ScrollPolicy.SweepStepMin, ScrollPolicy.SweepStepMax))
            {
                sp.SetScrollPercent(-1, target);
                Thread.Sleep(40);
                if (!el.IsOffscreen)
                {
                    T($"bring-row-into-view {Describe(el)}: swept container to {target:0.#}%");
                    return;
                }
            }
            T($"bring-row-into-view {Describe(el)}: still offscreen after a full sweep");
        }
        catch (Exception e) { T($"bring-row-into-view threw {e.GetType().Name}: {e.Message}"); }
    }

    /// <summary>The <c>options</c> attr's body for a ComboBox: every ListItem's
    /// Name, in tree (= model) order, JSON-encoded. A collapsed WinUI 3 ComboBox
    /// does not reliably realize its items for UIA, and a large popup
    /// virtualizes, so this reads the list the same way <see cref="Select"/> finds
    /// a target: expand, scan, page the popup down and rescan until the scroll
    /// stops moving, then restore the collapsed state it found. Items are
    /// de-duplicated by RuntimeId, never by name, so two options that paint the
    /// same text both survive — the injectivity check this attr exists for
    /// needs exactly that. A picker that cannot expand (disabled) answers
    /// whatever its collapsed subtree exposes.</summary>
    private string OptionsJson(AutomationElement combo)
    {
        var cf = _session.Automation.ConditionFactory;
        var byItem = cf.ByControlType(FlaUI.Core.Definitions.ControlType.ListItem);
        var names = new List<string>();
        var seen = new HashSet<string>();
        void Collect()
        {
            foreach (var item in combo.FindAllDescendants(byItem))
            {
                string key;
                try { key = string.Join(".", item.Properties.RuntimeId.ValueOrDefault ?? []); }
                catch { continue; }
                if (!seen.Add(key)) continue;
                try { names.Add(item.Name ?? ""); } catch { names.Add(""); }
            }
        }

        var expanded = false;
        if (combo.Patterns.ExpandCollapse.IsSupported && combo.IsEnabled)
        {
            var state = combo.Patterns.ExpandCollapse.Pattern.ExpandCollapseState.ValueOrDefault;
            if (state == FlaUI.Core.Definitions.ExpandCollapseState.Collapsed)
            {
                combo.Patterns.ExpandCollapse.Pattern.Expand();
                expanded = true;
            }
        }
        try
        {
            if (expanded)
            {
                // The popup's items realize a beat after Expand(); poll the rendered
                // tree (convention 14), not a fixed sleep. An empty picker simply
                // runs the deadline out and answers "[]".
                var deadline = DateTime.UtcNow + TimeSpan.FromSeconds(2);
                while (DateTime.UtcNow < deadline && combo.FindAllDescendants(byItem).Length == 0)
                    Thread.Sleep(100);
            }
            Collect();
            if (expanded && combo.Patterns.Scroll.IsSupported)
            {
                var scroll = combo.Patterns.Scroll.Pattern;
                var lastPct = double.MinValue;
                for (var step = 0; step < 60 && scroll.VerticallyScrollable.ValueOrDefault == true; step++)
                {
                    var pct = scroll.VerticalScrollPercent.ValueOrDefault;
                    if (pct >= 100.0 || Math.Abs(pct - lastPct) < 0.01) break;
                    lastPct = pct;
                    scroll.Scroll(FlaUI.Core.Definitions.ScrollAmount.NoAmount,
                        FlaUI.Core.Definitions.ScrollAmount.LargeIncrement);
                    Thread.Sleep(150); // let the newly-scrolled-in items realize
                    Collect();
                }
            }
        }
        finally
        {
            if (expanded)
            {
                try { combo.Patterns.ExpandCollapse.Pattern.Collapse(); } catch { }
            }
        }
        return System.Text.Json.JsonSerializer.Serialize(names);
    }

    /// <summary>Walk up the UIA tree to the nearest ancestor that advertises a
    /// vertical ScrollPattern (the hosting ScrollViewer).</summary>
    private static AutomationElement? FindScrollableAncestor(AutomationElement el)
    {
        var cur = el;
        for (var i = 0; i < 30 && cur is not null; i++)
        {
            try
            {
                if (cur.Patterns.Scroll.IsSupported &&
                    cur.Patterns.Scroll.Pattern.VerticallyScrollable.ValueOrDefault)
                    return cur;
            }
            catch { }
            try { cur = cur.Parent; }
            catch { break; }
        }
        return null;
    }

    /// <summary>
    /// Bring an element into the viewport of its nearest scrollable ancestor until
    /// at least <paramref name="minFraction"/> of its own bounding-rect AREA overlaps
    /// the ancestor's viewport rect — not merely "any part visible", which is all
    /// <see cref="ScrollIntoView"/> (and UIA's own <c>IsOffscreen</c>) guarantee.
    /// <c>IsOffscreen</c> flips false as soon as a small sliver enters the viewport
    /// (empirically as low as ~25% in a typical feed post-card layout, traced live),
    /// so a caller that needs a specific mid-list visibility — e.g. a dwell test
    /// proving a real "substantially visible" exposure, not merely "on screen at
    /// all" — has had no primitive to reach for; this is that
    /// primitive.
    ///
    /// A <c>SetScrollPercent</c> sweep too, but deliberately its OWN fixed 5% step —
    /// NOT <see cref="ScrollIntoView"/>'s <see cref="ScrollPolicy.SweepStepViewportFraction"/>
    /// policy. That policy is sized for DISCOVERY (any part onscreen is a hit,
    /// so a step of a full viewport is provably gap-free); this method's exit
    /// condition is a measured AREA fraction, where the step size directly bounds
    /// measurement precision — a step here jumps the achieved-fraction reading by
    /// up to a whole step's worth between samples, so shrinking it trades cost for
    /// precision the same way the sweep's step trades cost for discovery margin,
    /// just against a different threshold. Returns the achieved fraction once the
    /// sweep ends (whether or not the threshold was reached), or null when the
    /// element itself is never found.
    /// </summary>
    public double? ScrollIntoViewFraction(
        string id, double minFraction, int index = 0, IReadOnlyList<ScopeStep>? scope = null)
    {
        try
        {
            var root = _session.RootElement;
            var cf = _session.Automation.ConditionFactory;
            var scoped = ElementFinder.WalkScope(root, cf, scope);
            AutomationElement el;
            try { el = ElementFinder.FindOne(scoped, cf, id, index); }
            catch (ElementNotFoundException) { return null; }

            var container = FindScrollableAncestor(el);
            var fraction = VisibleFraction(el, container);
            if (fraction >= minFraction || container is null) return fraction;

            var sp = container.Patterns.Scroll.Pattern;
            if (!sp.VerticallyScrollable.ValueOrDefault) return fraction;

            for (double p = 0; p <= 100.0; p += 5.0)
            {
                try { sp.SetScrollPercent(-1, p); } // -1 = leave horizontal as-is
                catch { break; }
                Thread.Sleep(40);
                fraction = VisibleFraction(el, container);
                if (fraction >= minFraction) return fraction;
            }
            return fraction;
        }
        catch
        {
            return null;
        }
    }

    /// <summary>
    /// Fraction of <paramref name="el"/>'s own bounding-rect AREA that overlaps
    /// <paramref name="viewport"/>'s bounding-rect (both screen coordinates) — 0
    /// when off entirely, 1 when fully inside. Reads only X/Y/Width/Height (the
    /// members <see cref="DumpNode"/> already relies on) rather than a Rect's
    /// Left/Right/Top/Bottom, so this makes no new assumption about the FlaUI
    /// bounding-rect type's surface. <paramref name="viewport"/> null (no
    /// scrollable ancestor found) falls back to the element's own
    /// <c>IsOffscreen</c>: 1 if on, 0 if off — the best this primitive can do with
    /// nothing to measure against.
    /// </summary>
    private static double VisibleFraction(AutomationElement el, AutomationElement? viewport)
    {
        if (viewport is null) return el.IsOffscreen ? 0.0 : 1.0;
        double elX, elY, elW, elH;
        try
        {
            var r = el.BoundingRectangle;
            elX = r.X; elY = r.Y; elW = r.Width; elH = r.Height;
        }
        catch { return 0.0; }
        if (elW <= 0 || elH <= 0) return 0.0;
        double vpX, vpY, vpW, vpH;
        try
        {
            var r = viewport.BoundingRectangle;
            vpX = r.X; vpY = r.Y; vpW = r.Width; vpH = r.Height;
        }
        catch { return el.IsOffscreen ? 0.0 : 1.0; }
        var ix = Math.Max(0, Math.Min(elX + elW, vpX + vpW) - Math.Max(elX, vpX));
        var iy = Math.Max(0, Math.Min(elY + elH, vpY + vpH) - Math.Max(elY, vpY));
        return (ix * iy) / (elW * elH);
    }

    /// <summary>
    /// Read the OS clipboard's Unicode text, via the raw Win32 clipboard API
    /// rather than the OLE-based <c>System.Windows.Forms.Clipboard</c> /
    /// <c>Windows.ApplicationModel.DataTransfer.Clipboard</c> wrappers — those
    /// require an STA thread and (the WinRT one) package identity, neither of
    /// which this plain console bridge process has. <c>OpenClipboard</c> can
    /// transiently fail if another process (e.g. the app itself, mid-write)
    /// holds the clipboard, so retry briefly before giving up. Returns null if
    /// the clipboard has no CF_UNICODETEXT content.
    /// </summary>
    public string? GetClipboardText()
    {
        var deadline = DateTime.UtcNow + TimeSpan.FromSeconds(2);
        while (true)
        {
            if (Win32.OpenClipboard(IntPtr.Zero))
            {
                try
                {
                    var handle = Win32.GetClipboardData(Win32.CF_UNICODETEXT);
                    if (handle == IntPtr.Zero) return null;
                    var ptr = Win32.GlobalLock(handle);
                    if (ptr == IntPtr.Zero) return null;
                    try
                    {
                        return System.Runtime.InteropServices.Marshal.PtrToStringUni(ptr);
                    }
                    finally
                    {
                        Win32.GlobalUnlock(handle);
                    }
                }
                finally
                {
                    Win32.CloseClipboard();
                }
            }
            if (DateTime.UtcNow >= deadline)
                throw new InvalidOperationException(
                    "GetClipboardText: could not OpenClipboard (another process held it for 2s)");
            Thread.Sleep(50);
        }
    }

    public string Screenshot(string name)
    {
        var dir = Path.Combine(Path.GetTempPath(), "fauna-e2e-screenshots");
        Directory.CreateDirectory(dir);
        var path = Path.Combine(dir, $"{name}.png");
        var img = Capture.Screen();
        img.ToFile(path);
        return path;
    }

    /// <summary>
    /// Dismiss system dialogs (e.g. Windows Firewall) by searching the desktop
    /// for known dialog patterns and clicking Cancel/Allow.
    /// Returns the number of dialogs dismissed.
    /// </summary>
    public int DismissSystemDialogs()
    {
        var automation = _session.Automation;
        var desktop = automation.GetDesktop();
        var cf = automation.ConditionFactory;
        int dismissed = 0;

        // Windows Firewall dialog — click Cancel to allow localhost-only.
        // Best-effort BY DESIGN (the dialog is rare — most launches have none to
        // dismiss), so a failed scan must never fail the launch it's guarding.
        // A full-desktop FindAllChildren is a genuine COM/UIA round trip over
        // EVERY top-level window, and on a busy shared dev machine (several
        // concurrent build/test sessions is normal) it can hit UIA's own
        // internal timeout and throw (observed live: COMException 0x800705B4
        // "the timeout period
        // expired"), which used to propagate straight through launch() as a
        // fatal RuntimeError over a dialog that was never even there.
        AutomationElement[] firewallDialogs;
        try
        {
            firewallDialogs = desktop.FindAllChildren(
                cf.ByClassName("#32770"));  // Standard Windows dialog class
        }
        catch (System.Runtime.InteropServices.COMException)
        {
            return 0;
        }
        foreach (var dialog in firewallDialogs)
        {
            // Check if it's a firewall dialog by looking for known text
            var textBlocks = dialog.FindAllDescendants(cf.ByControlType(FlaUI.Core.Definitions.ControlType.Text));
            bool isFirewall = false;
            foreach (var tb in textBlocks)
            {
                if (tb.Name?.Contains("firewall", StringComparison.OrdinalIgnoreCase) == true
                    || tb.Name?.Contains("allow", StringComparison.OrdinalIgnoreCase) == true
                    || tb.Name?.Contains("network", StringComparison.OrdinalIgnoreCase) == true)
                {
                    isFirewall = true;
                    break;
                }
            }
            if (!isFirewall) continue;

            // Click Cancel button
            var cancelBtn = dialog.FindFirstDescendant(cf.ByName("Cancel"));
            if (cancelBtn is not null)
            {
                cancelBtn.Click();
                dismissed++;
                Thread.Sleep(500);
            }
        }
        return dismissed;
    }

    /// <summary>
    /// Dump a UIA subtree as JSON. Diagnostic-only — no test asserts on it; it exists
    /// so a stuck session can *look at the tree* instead of guessing at properties
    /// (the lesson of five-hypothesis elimination table).
    /// </summary>
    /// <param name="depthStr">Max walk depth below the anchor.</param>
    /// <param name="anchor">Optional AutomationId or ClassName to root the dump at
    /// (e.g. <c>NavigationView</c>), keeping the payload readable.</param>
    /// <param name="raw">Walk the UIA <b>raw</b> view instead of the default search.
    /// This is the discriminator that matters: <c>FindAllChildren</c> answers "what can
    /// a test see", the raw walker answers "does the peer exist at all". An element in
    /// the raw dump but absent from the default one is <i>realized but pruned from the
    /// control view</i>; absent from both means <i>never realized</i>. Those two have
    /// completely different fixes, and no property guess can tell them apart.</param>
    public object DumpTree(string depthStr, string? anchor = null, bool raw = false)
    {
        int maxDepth = int.TryParse(depthStr, out var d) ? d : 3;
        var root = _session.RootElement;
        if (!string.IsNullOrWhiteSpace(anchor))
        {
            var cf = _session.Automation.ConditionFactory;
            var found = root.FindFirstDescendant(cf.ByAutomationId(anchor))
                        ?? root.FindFirstDescendant(cf.ByClassName(anchor));
            if (found is null)
                return new Dictionary<string, object?> { ["anchorNotFound"] = anchor };
            root = found;
        }
        var walker = raw ? _session.Automation.TreeWalkerFactory.GetRawViewWalker() : null;
        return DumpNode(root, 0, maxDepth, walker);
    }

    private static object DumpNode(AutomationElement el, int depth, int maxDepth,
        FlaUI.Core.ITreeWalker? walker)
    {
        var node = new Dictionary<string, object?>();
        try { node["name"] = el.Name; } catch { node["name"] = "?"; }
        try { node["automationId"] = el.AutomationId; } catch { node["automationId"] = "?"; }
        try { node["className"] = el.ClassName; } catch { node["className"] = "?"; }
        try { node["controlType"] = el.ControlType.ToString(); } catch { node["controlType"] = "?"; }
        try { node["isOffscreen"] = el.IsOffscreen; } catch { node["isOffscreen"] = null; }
        // The three properties that separate "absent" from "present but unseeable".
        // A zero-size rect means the item exists but its parent laid it out at 0×0 (the
        // stale-measure shape); IsControlElement=false means UIA itself prunes it from
        // the control view a test searches.
        try
        {
            var r = el.BoundingRectangle;
            node["rect"] = $"{r.X},{r.Y},{r.Width}x{r.Height}";
        }
        catch { node["rect"] = "?"; }
        try { node["isControlElement"] = el.Properties.IsControlElement.ValueOrDefault; }
        catch { node["isControlElement"] = null; }
        try { node["isContentElement"] = el.Properties.IsContentElement.ValueOrDefault; }
        catch { node["isContentElement"] = null; }
        if (depth < maxDepth)
        {
            try
            {
                var children = walker is null ? el.FindAllChildren() : RawChildren(el, walker);
                if (children.Length > 0)
                    node["children"] = children
                        .Select(c => DumpNode(c, depth + 1, maxDepth, walker)).ToArray();
            }
            catch { node["childrenError"] = true; }
        }
        return node;
    }

    /// <summary>Enumerate children via an explicit tree walker (the raw view), which
    /// surfaces peers the condition-based <c>FindAllChildren</c> search does not.</summary>
    private static AutomationElement[] RawChildren(AutomationElement el, FlaUI.Core.ITreeWalker walker)
    {
        var list = new List<AutomationElement>();
        var child = walker.GetFirstChild(el);
        // Bounded: a malformed/cyclic peer chain must not hang the bridge.
        for (int i = 0; child is not null && i < 512; i++)
        {
            list.Add(child);
            child = walker.GetNextSibling(child);
        }
        return list.ToArray();
    }

    private AutomationElement Find(string id, int index = 0, IReadOnlyList<ScopeStep>? scope = null)
    {
        var root = _session.RootElement;
        var cf = _session.Automation.ConditionFactory;
        var scoped = ElementFinder.WalkScope(root, cf, scope);
        return ElementFinder.FindOne(scoped, cf, id, index);
    }

    private AutomationElement FindWithRetry(string id, int index, TimeSpan timeout,
        IReadOnlyList<ScopeStep>? scope = null)
    {
        var deadline = DateTime.UtcNow + timeout;
        while (true)
        {
            try
            {
                return Find(id, index, scope);
            }
            catch (ElementNotFoundException) when (DateTime.UtcNow < deadline)
            {
                Thread.Sleep(200);
            }
        }
    }

    /// <summary>
    /// Bring the FaunaApp main window to the foreground so a subsequent
    /// SendInput is accepted. Best-effort — swallows failures (a missing HWND
    /// must not turn into a bridge 500; <see cref="SendPhysicalInput"/>'s retry
    /// covers transient foreground loss). UIA SetFocus is not a substitute: it
    /// activates the containing window but does not reliably make SendInput
    /// acceptable, so we still need Win32 SetForegroundWindow on the app HWND.
    ///
    /// <para>⚠ The converse — "SetFocus doesn't touch the foreground" — was the
    /// standing belief here until <c>test_flaui_input_lock_windows.py</c> measured
    /// the opposite; that is why focus is now lock-held too
    /// (<see cref="FocusThenInject"/>).</para>
    /// </summary>
    private void ForegroundApp()
    {
        WithInputLock(() =>
        {
            try
            {
                var hwnd = (IntPtr)_session.RootElement.Properties.NativeWindowHandle.ValueOrDefault;
                Win32.ForceForeground(hwnd);
            }
            catch { }
        });
    }

    /// <summary>
    /// Run a keyboard ceremony — UIA focus, the focus-landing click, and the caller's
    /// injection — as ONE critical section.
    ///
    /// <para><b>Why focus belongs inside the lock.</b> The mutex wrapped only
    /// <see cref="ForegroundApp"/>+<c>SendInput</c> and left every <c>el.Focus()</c>
    /// outside it, on the standing belief (still written at
    /// <see cref="SendPhysicalInput"/>) that "UIA SetFocus alone doesn't change
    /// foreground". **That belief is false**, and
    /// <c>test_flaui_input_lock_windows.py</c> measured it: with two bridges, A took
    /// the lock, won the foreground (<c>app_hwnd == GetForegroundWindow()</c>), and
    /// held it — then B, still BLOCKED on the mutex, called <c>el.Focus()</c>, which
    /// ACTIVATED B's window; A's ten keystrokes duly landed in app B. Serializing the
    /// injection is not enough when an unserialized call can move the foreground out
    /// from under the holder: SetFocus activates the containing window, so it is a
    /// foreground-mutating operation and belongs under the same lock.</para>
    ///
    /// <para>Reentrancy makes this cheap: the nested
    /// <see cref="SendPhysicalInput"/> calls re-acquire on the same thread (Win32
    /// mutexes are thread-owned) and each acquire has its own release.</para>
    /// </summary>
    private void FocusThenInject(AutomationElement el, Action inject, bool clickOnlyIfUnfocused = false)
    {
        WithInputLock(() =>
        {
            // Two focus attempts: UIA SetFocus first (the cheap path), then a
            // physical click (lands keyboard focus on a TextBox where SetFocus alone
            // doesn't, e.g. when the value was last written via ValuePattern). Both
            // best-effort — a missing window foreground must not become a bridge 500.
            TakeFocus(el, $"focus before physical input into {Describe(el)}");
            try
            {
                var inner = FindInnerEdit(el) ?? el;
                // A click in a text field MOVES ITS CARET to the click point. For a
                // caret-navigation key that destroys the very state the key acts on:
                // every ArrowLeft on the compose field clicked its centre first, so the
                // caret snapped back mid-text before each press and could never step
                // (measured: 17 → 11 on the first press, then 11 for every later one).
                // PressKey therefore clicks only when SetFocus did not land focus.
                var focused = clickOnlyIfUnfocused
                    && (inner.Properties.HasKeyboardFocus.ValueOrDefault
                        || el.Properties.HasKeyboardFocus.ValueOrDefault);
                // ONE attempt, not the full SendPhysicalInput retry loop
                // (, judged): this click is already
                // best-effort (caught below either way), and on a disconnected
                // session every attempt hits the same "no attached desktop" wall
                // the retry cannot get past — the 5-attempt backoff would burn
                // ~800ms of THIS ceremony's budget, on every keyboard-input call,
                // for no behavioral difference from failing once.
                ForegroundApp();
                if (!focused) inner.Click();
            }
            catch { }
            inject();
        });
    }

    /// <summary>
    /// Commit an editable control's already-written value — the cross-app "click an
    /// editable to commit" contract (<c>actions/backups.py</c>'s <c>set_member_cap</c>,
    /// mirroring linux/tui's GTK SpinButton-activate idiom).
    ///
    /// <para><b>UIA focus-shift first, physical Enter only as the fallback.</b> Enter
    /// is <c>SendInput</c>, and <c>SendInput</c> needs more than foreground: it needs
    /// an ATTACHED INTERACTIVE DESKTOP. When this box's automation session runs
    /// disconnected (<c>query session</c> → <c>Disc</c>), every <c>SendInput</c> call
    /// fails <c>Win32Exception(5)</c> "Access is denied" no matter how correct the
    /// foreground-stealing is — measured
    /// against an unmodified tree, and the reason
    /// <c>test_folder_member_role_and_cap_editing</c> stayed gated off windows.
    /// A UIA focus shift has no such requirement (it is a COM call into the provider,
    /// not a hardware-input injection), so the commit path no longer depends on the
    /// desktop's attachment state at all. Convention 14 (`testing.md`): this REMOVES
    /// the dependency rather than widening a retry ceiling around it.</para>
    ///
    /// <para><b>Why a focus shift commits.</b> Every WinUI editable in FaunaApp that
    /// has a commit trigger commits on <c>LostFocus</c> — all three of them
    /// (<c>FoldersPage</c>'s <c>folder-member-cap-input</c>,
    /// <c>MailExportPanel</c>'s two date inputs), each a "write this field's value"
    /// handler. Blurring the field is therefore the app's own commit gesture, not a
    /// harness trick. Nothing in the app reacts to GAINING focus: FaunaApp declares
    /// zero <c>GotFocus</c>/<c>GettingFocus</c>/<c>LosingFocus</c> handlers, so
    /// parking focus on a neighbour cannot fire product logic — the safety question
    /// this path has to answer, answered by grep rather than by hope.</para>
    ///
    /// <para>The physical Enter stays as the fallback so a box where the focus shift
    /// does not take behaves exactly as it did before this path existed.</para>
    /// </summary>
    private void CommitEditable(AutomationElement el)
    {
        var why = TryCommitByFocusShift(el);
        if (why is null) return;
        // FocusThenInject, not a bare SendPhysicalInput: a background FaunaApp
        // window's plain Keyboard.Press throws Win32Exception(5) "Access is denied"
        // (SendInput needs foreground) — FocusThenInject re-focuses + foregrounds
        // under the shared input lock first, same as every other keyboard injection
        // in this file.
        try
        {
            FocusThenInject(el, () => SendPhysicalInput(() => Keyboard.Press(VirtualKeyShort.ENTER),
                $"physical Enter to commit {Describe(el)}"));
        }
        catch (Exception ex)
        {
            // Convention 6: the failure carries its own diagnosis. Both halves of
            // this path can fail for unrelated reasons, and the bridge's stderr —
            // where the focus-shift narration goes — is NOT part of the HTTP 500 a
            // test sees, so a bare "Access is denied" leaves the reader unable to
            // tell "the UIA path was never tried" from "it was tried and declined".
            throw new InvalidOperationException(
                $"commit of {Describe(el)} failed. The UIA focus-shift commit was not "
                    + $"used: {why}. The physical-Enter fallback then failed: {ex.Message}",
                ex);
        }
    }

    /// <summary>
    /// Focus <paramref name="el"/>, then move focus to its nearest keyboard-focusable
    /// neighbour, firing the app's <c>LostFocus</c> commit. Returns <c>null</c> when
    /// the shift was performed, or a one-line reason the caller puts in front of the
    /// physical-Enter fallback's own failure.
    ///
    /// <para><b>Why the shift is attempted, not gated on a focus read.</b> The first
    /// cut DID gate it — claim a commit only if focus provably landed and then left —
    /// and it vetoed every attempt, falling straight back to the Enter that cannot
    /// work. The reason is worth recording, because the obvious repair is the wrong
    /// one: the guard asked <c>Automation.FocusedElement()</c>, which answers the
    /// DESKTOP's focused element, so on a window that is not foreground it names
    /// something in another app and "is focus inside our field" is false no matter
    /// where XAML focus actually sits. The per-element <c>HasKeyboardFocus</c> read is
    /// the honest one and does track it (measured 2026-09-03 on a DISCONNECTED
    /// session, window not foreground: True on the field after
    /// <see cref="AutomationElement.Focus"/>, False after the shift). It is still kept
    /// DIAGNOSTIC rather than promoted to a gate — one box in one state is not enough
    /// to bet the path on, and a gate that vetoes wrongly costs exactly what the first
    /// cut cost. The bridge attempts the gesture; THE TEST is the witness that it took
    /// (<c>test_folder_member_role_and_cap_editing</c> asserts the warning clears AND
    /// the cap survives a nest round-trip), the same division of labour every UIA path
    /// here already has — <c>Invoke</c> does not verify that its command ran either.
    /// A no-op shift cannot yield a false green: nothing downstream would change.</para>
    ///
    /// <para>Focus is lock-held because <c>SetFocus</c> ACTIVATES the containing
    /// window and is therefore foreground-mutating — the boundary
    /// <see cref="FocusThenInject"/> documents and
    /// <c>test_flaui_input_lock_windows.py</c> measured. For the same reason this
    /// path is a recorded foreground take (<see cref="RecordForegroundTake"/>): it
    /// needs no attached desktop, but it does take the keyboard focus. UIA has no
    /// focus-free way to fire <c>LostFocus</c>, and it is reached only from gestures
    /// that already fell back to physical input (a <see cref="Click"/> on an
    /// editable, <see cref="Type"/>'s newline path), so it stays one of e2e
    /// convention 10's named foregrounding fallbacks rather than a UIA path.</para>
    /// </summary>
    private string? TryCommitByFocusShift(AutomationElement el)
    {
        var target = FindBlurTarget(el);
        if (target is null)
            return $"no keyboard-focusable blur target near {Describe(el)}";

        string? failure = null;
        RecordForegroundTake($"focus shift to commit {Describe(el)}");
        WithInputLock(() =>
        {
            try { el.Focus(); }
            catch (Exception ex)
            {
                failure = $"SetFocus on {Describe(el)} threw {ex.GetType().Name}: {ex.Message}";
                return;
            }
            var landed = ReadsAsFocused(el);
            try { target.Focus(); }
            catch (Exception ex)
            {
                failure = $"SetFocus on blur target {Describe(target)} threw "
                    + $"{ex.GetType().Name}: {ex.Message}";
                return;
            }
            // `[bridge]` prefix, not `[flaui-bridge]`: that is the prefix
            // WindowsBridgeDriver._diagnostic_lines keeps when it surfaces bridge
            // output, and a diagnostic filtered out of every report is not one.
            Console.Error.WriteLine(
                $"[bridge] commit-by-focus-shift: {Describe(el)} → {Describe(target)} "
                    + $"(HasKeyboardFocus: on-field {landed}, after-shift "
                    + $"{ReadsAsFocused(el)} — diagnostic only, never a gate; see "
                    + "TryCommitByFocusShift)");
        });
        return failure;
    }

    /// <summary>Whether UIA reports keyboard focus on <paramref name="el"/> or inside
    /// it (a composite takes focus on its inner <c>Edit</c>). DIAGNOSTIC ONLY — never
    /// a gate; see <see cref="TryCommitByFocusShift"/>. Note this is the per-element
    /// property, NOT <c>Automation.FocusedElement()</c> — that one answers the
    /// desktop's focus and is useless from a non-foreground window.</summary>
    private bool ReadsAsFocused(AutomationElement el)
    {
        try { if (el.Properties.HasKeyboardFocus.ValueOrDefault) return true; }
        catch { }
        try
        {
            var inner = FindInnerEdit(el);
            if (inner is not null && inner.Properties.HasKeyboardFocus.ValueOrDefault) return true;
        }
        catch { }
        return false;
    }

    /// <summary>
    /// The NEAREST keyboard-focusable element that is neither <paramref name="el"/>
    /// nor inside it — found by walking <paramref name="el"/>'s ancestors and looking
    /// at each one's CHILDREN, so the blur target stays a neighbour rather than
    /// "some focusable control elsewhere in the window".
    ///
    /// <para>Generic on purpose: naming a specific sibling (the member row's
    /// role-select, say) would encode one page's layout into the bridge, and the next
    /// editable with a commit trigger would need its own special case. Proximity is
    /// also the safety property — the further focus travels, the more of the tree it
    /// touches.</para>
    ///
    /// <para>⚠ The neighbour it picks can be a control you would never want ACTIVATED
    /// — on the folder member row it resolves to `folder-member-remove-button`. That
    /// is safe, and the reason has to be held in mind whenever this is edited: giving
    /// a control keyboard focus is not invoking it, and FaunaApp declares no
    /// `GotFocus`/`GettingFocus` handler anywhere, so nothing observes the arrival.
    /// Anything that ever makes focus itself actionable (an autocomplete that opens on
    /// focus, a GotFocus handler) breaks that assumption and this search would then
    /// need a control-type filter.</para>
    ///
    /// <para>Children, not descendants, at each level: WinUI 3 UIA ignores the search
    /// root for <c>FindAllDescendants</c> and answers with whole-window matches (the
    /// escape <c>ElementFinder.WithinSubtree</c> exists to re-scope), so a
    /// descendants query here would quietly mean "anywhere". Walking up one level at a
    /// time reaches a nested candidate when we reach ITS parent, in nearest-first
    /// order. The window-wide sweep is only the last resort, re-scoped client-side.</para>
    /// </summary>
    private AutomationElement? FindBlurTarget(AutomationElement el)
    {
        AutomationElement? cur;
        try { cur = el.Parent; }
        catch { return null; }

        var rootHwnd = (IntPtr)_session.RootElement.Properties.NativeWindowHandle.ValueOrDefault;
        for (var level = 0; level < 16 && cur is not null; level++)
        {
            var hit = FirstFocusableOutside(SafeChildren(cur), el);
            if (hit is not null) return hit;
            if ((IntPtr)cur.Properties.NativeWindowHandle.ValueOrDefault == rootHwnd) break;
            try { cur = cur.Parent; }
            catch { break; }
        }

        // Last resort: anything focusable in this window. Re-scoped client-side
        // because of the same WinUI descendants escape noted above.
        try
        {
            var root = _session.RootElement;
            return FirstFocusableOutside(
                root.FindAllDescendants().Where(d => IsWithin(d, root)).ToArray(), el);
        }
        catch { return null; }
    }

    private static AutomationElement[] SafeChildren(AutomationElement el)
    {
        try { return el.FindAllChildren(); }
        catch { return Array.Empty<AutomationElement>(); }
    }

    private static AutomationElement? FirstFocusableOutside(
        IEnumerable<AutomationElement> candidates, AutomationElement el)
    {
        foreach (var c in candidates)
        {
            // `el` itself and its own inner peers (an AutoSuggestBox's Edit child)
            // are focus SINKS for this purpose: focusing one of them would keep
            // focus inside the field and fire no LostFocus.
            if (IsWithin(c, el)) continue;
            try { if (!c.Properties.IsKeyboardFocusable.ValueOrDefault) continue; }
            catch { continue; }
            return c;
        }
        return null;
    }

    /// <summary>Walk <paramref name="el"/>'s parent chain (itself first) to a bounded
    /// depth; true iff <paramref name="ancestor"/> is reached. Mirrors
    /// <c>ElementFinder.IsWithin</c> (private there).</summary>
    private static bool IsWithin(AutomationElement el, AutomationElement ancestor)
    {
        var cur = el;
        for (var i = 0; i < 64 && cur is not null; i++)
        {
            if (SameEl(cur, ancestor)) return true;
            try { cur = cur.Parent; }
            catch { return false; }
        }
        return false;
    }

    /// <summary>UIA-identity compare via <c>RuntimeId</c>, falling back to FlaUI's
    /// <c>Equals</c>. Mirrors <c>ElementFinder.SameElement</c> (private there).</summary>
    private static bool SameEl(AutomationElement a, AutomationElement b)
    {
        try
        {
            var ra = a.Properties.RuntimeId.ValueOrDefault;
            var rb = b.Properties.RuntimeId.ValueOrDefault;
            if (ra is not null && rb is not null) return ra.SequenceEqual(rb);
        }
        catch { }
        return a.Equals(b);
    }

    /// <summary>A short, log-safe identity for an element: its AutomationId when it
    /// has one, else control type + Name. Used only in the commit-path diagnostics —
    /// a fallback that fires must say WHICH element it gave up on (convention 6).</summary>
    private static string Describe(AutomationElement el)
    {
        try
        {
            var id = el.Properties.AutomationId.ValueOrDefault;
            if (!string.IsNullOrEmpty(id)) return id;
        }
        catch { }
        try
        {
            var name = el.Properties.Name.ValueOrDefault;
            var type = el.Properties.ControlType.ValueOrDefault;
            return string.IsNullOrEmpty(name) ? $"<{type}>" : $"<{type} '{name}'>";
        }
        catch { return "<unknown element>"; }
    }

    /// <summary>
    /// Serializes every physical-input critical section across ALL FlaUI bridge
    /// processes on this desktop. Session-scoped (<c>Local\</c>) on purpose:
    /// foreground ownership and <c>SendInput</c> are themselves per-session, so a
    /// <c>Global\</c> name would over-serialize sessions that cannot interfere.
    ///
    /// <para><b>Why a mutex and not more retrying.</b> <see cref="SendPhysicalInput"/>'s
    /// error-5 retry already handles the case where a sibling steals foreground from
    /// US. It cannot handle the INVERSE, which is the damaging one: when we win the
    /// foreground race, a sibling's in-flight <c>Keyboard.Type</c> lands in OUR app.
    /// That arrives as a keystroke nobody in this process sent — an ESC closes a
    /// <c>ContentDialog</c> with its confirm handler never run, so the ceremony
    /// silently no-ops: modal closed, no RPC, no error, product code blameless.
    /// It is indistinguishable from a lost RPC without the app-side ceremony
    /// trace. Retrying protects the sender; only
    /// serializing the whole ceremony protects the victim.</para>
    ///
    /// <para><b>Scope of the critical section — foreground+inject is NOT enough.</b>
    /// Every operation that can move the foreground has to be inside it, and UIA
    /// <c>SetFocus</c> is one of those (it activates the containing window). That mutex
    /// wrapped only foreground+inject; a sibling BLOCKED on this mutex could still
    /// steal the foreground with an unlocked <c>el.Focus()</c> and collect the
    /// holder's keystrokes — measured, not reasoned, by
    /// <c>test_flaui_input_lock_windows.py</c>. <see cref="FocusThenInject"/> is the
    /// corrected boundary; add new foreground-mutating operations there, not around
    /// the injection alone.</para>
    ///
    /// <para>Convention 14 (`testing.md`): a harness mutex is a CAUSAL fix — it
    /// removes the collision — as opposed to widening a timeout, which would only
    /// have made a corrupted ceremony fail later.</para>
    /// </summary>
    private static readonly Mutex _inputMutex = new(false, @"Local\FaunaE2ePhysicalInput");

    /// <summary>
    /// <c>FAUNA_E2E_INPUT_LOCK=off</c> runs every physical-input section
    /// UNSERIALIZED. Its only sanctioned use is the red half of
    /// <c>tests/test_flaui_input_lock_windows.py</c>: a lock test that passes with
    /// the lock disabled proves nothing, so the failing mode has to be reachable on
    /// demand rather than by hand-editing this file and remembering to undo it —
    /// which is exactly how a mechanism ends up with a test that never actually
    /// exercised it.
    ///
    /// <para>⚠ Never set it for an ordinary run: two concurrent FlaUI sessions will
    /// then inject keystrokes into each other's apps, which is the defect
    /// <see cref="_inputMutex"/> exists to remove.</para>
    /// </summary>
    private static readonly bool _inputLockDisabled = string.Equals(
        Environment.GetEnvironmentVariable("FAUNA_E2E_INPUT_LOCK"), "off",
        StringComparison.OrdinalIgnoreCase);

    private static int _unlockedWarningPrinted;

    /// <summary>
    /// Run <paramref name="body"/> holding <see cref="_inputMutex"/>. Reentrant on
    /// the same thread (Win32 mutexes are thread-owned), so the nested
    /// <see cref="ForegroundApp"/> call inside <see cref="SendPhysicalInput"/> is
    /// safe — each acquire has its own matching release.
    ///
    /// <para>Never blocks a test forever and never fails one: a 30 s ceiling
    /// (far above any real input section, which is milliseconds) degrades to
    /// running UNLOCKED rather than throwing, and an
    /// <see cref="AbandonedMutexException"/> — a bridge that died mid-input — means
    /// we now own it and may proceed, which is exactly the self-healing we want on
    /// a shared box.</para>
    /// </summary>
    private static void WithInputLock(Action body)
    {
        if (_inputLockDisabled)
        {
            // Once per bridge process — loud enough that a run cannot be UNSERIALIZED
            // without saying so in the drained bridge log, quiet enough not to bury it.
            if (System.Threading.Interlocked.Exchange(ref _unlockedWarningPrinted, 1) == 0)
                Console.Error.WriteLine(
                    "[flaui-bridge] WARNING: FAUNA_E2E_INPUT_LOCK=off — physical-input "
                        + "sections run UNSERIALIZED (diagnostic mode; see Actions._inputLockDisabled).");
            body();
            return;
        }

        bool held = false;
        try
        {
            try { held = _inputMutex.WaitOne(TimeSpan.FromSeconds(30)); }
            catch (AbandonedMutexException) { held = true; }
            if (!held)
            {
                // Convention 9 (`testing.md`): a machine-wide lock wait is LOUD.
                // Proceeding unlocked is the right call (failing the run would turn
                // a rare stall into a red), but it must never be silent — this line
                // is the only thing separating "the mutex protected us" from "the
                // mutex was bypassed" when a stray-input flake reappears. The
                // bridge's stdout/stderr is drained into the driver's `_bridge_log`.
                Console.Error.WriteLine(
                    "[flaui-bridge] WARNING: physical-input mutex not acquired within 30s — "
                        + "proceeding UNLOCKED; a sibling FlaUI session may inject input into "
                        + "this app (see Actions._inputMutex).");
            }
            body();
        }
        finally
        {
            if (held) { try { _inputMutex.ReleaseMutex(); } catch { } }
        }
    }

    /// <summary>
    /// Run a physical-input action (mouse SendInput via <c>el.Click()</c> or
    /// keyboard SendInput via <c>Keyboard.*</c>) after foregrounding the
    /// FaunaApp window, retrying on the Win32 "Access is denied" (error 5)
    /// that SendInput raises when another process owns foreground.
    ///
    /// On a shared desktop with parallel test runs, foreground ownership churns
    /// and a single SendInput can be denied transiently; re-foregrounding and
    /// retrying clears it (the documented `Actions.Clear`/`clear_and_type`
    /// flake). We retry ONLY on NativeErrorCode==5 so a genuine input failure
    /// (any other error) still surfaces immediately. UIA-pattern paths
    /// (Invoke/Toggle/ValuePattern) never reach here — this wraps only the
    /// physical fallback used when no pattern is available (composite controls
    /// like AutoSuggestBox, raw clicks).
    /// </summary>
    private void SendPhysicalInput(Action input, string what = "physical input")
    {
        // Once per gesture, not per retry attempt: the record answers "which
        // gestures took the foreground", and each attempt below re-foregrounds.
        RecordForegroundTake(what);
        // The WHOLE retry loop is one critical section, not each attempt: a
        // sibling that grabbed foreground between our attempts would restart the
        // very race the retry is trying to escape, and our backoff sleeps are
        // precisely when it would win.
        WithInputLock(() =>
        {
            const int maxAttempts = 5;
            for (int attempt = 1; ; attempt++)
            {
                ForegroundApp();
                try
                {
                    input();
                    return;
                }
                catch (System.ComponentModel.Win32Exception ex)
                    when (ex.NativeErrorCode == 5 && attempt < maxAttempts)
                {
                    // "Access is denied": foreground was stolen mid-SendInput —
                    // now only possible from a NON-bridge window (the installed
                    // FaunaApp, Explorer), since sibling bridges hold this lock.
                    // Back off (growing) and re-foreground on the next pass.
                    Thread.Sleep(80 * attempt);
                }
            }
        });
    }

    /// <summary>
    /// <see cref="SendPhysicalInput"/> for a gesture that has NO UIA equivalent —
    /// so unlike every UIA-preferring caller in this file, a failure here is not a
    /// fallback exhausted, it is the ONLY path exhausted. Converts the bare
    /// <see cref="System.ComponentModel.Win32Exception"/> "Access is denied" (error
    /// 5) <see cref="SendPhysicalInput"/>'s retry loop re-throws once exhausted into
    /// a self-diagnosing exception naming the cause (convention 6,
    /// `e2e-conventions.md` § 6 — the bridge's own stderr narration is not part of
    /// the HTTP 500 a test sees, so a bare "Access is denied" leaves the reader
    /// unable to tell a genuine input bug from this box's <c>Disc</c> automation
    /// session). : on a disconnected session
    /// (<c>query session</c> → <c>Disc</c>), SendInput requires an ATTACHED
    /// interactive desktop and fails this way regardless of how correct the
    /// foreground handling is (the project's internal Windows dev-setup notes,
    /// § Manually verifying cfapi on-demand hydration — the
    /// SendInput/attached-desktop paragraph) — the same
    /// mechanism <see cref="TryCommitByFocusShift"/>'s doc comment measured.
    /// </summary>
    private void PhysicalOnly(string what, Action input)
    {
        try
        {
            SendPhysicalInput(input, what);
        }
        catch (System.ComponentModel.Win32Exception ex) when (ex.NativeErrorCode == 5)
        {
            throw new InvalidOperationException(
                $"{what} has no UIA equivalent and requires the physical SendInput "
                    + "path, which failed with Win32 error 5 \"Access is denied\" after "
                    + "exhausting its foreground-retry budget. This is not a foreground "
                    + "problem: it almost always means the automation session has no "
                    + "ATTACHED interactive desktop (win.md's SendInput/attached-desktop "
                    + "paragraph) — SendInput needs one no matter how correct the "
                    + "foreground handling is.",
                ex);
        }
    }
}

/// <summary>
/// What <see cref="Actions.TypePhysical"/> observed about foreground ownership while
/// it held the input critical section. <see cref="AppHwnd"/> is this bridge's app;
/// the other two are whoever actually owned the foreground at each point.
/// </summary>
record PhysicalTypeReport(long AppHwnd, long ForegroundAfterForegroundApp, long ForegroundAtInject);

internal static class Win32
{
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    [return: System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)]
    public static extern bool SetForegroundWindow(IntPtr hWnd);

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);

    [System.Runtime.InteropServices.DllImport("kernel32.dll")]
    public static extern uint GetCurrentThreadId();

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    [return: System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)]
    public static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    [return: System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)]
    public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    public const int SW_RESTORE = 9;

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    [return: System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)]
    public static extern bool PostMessageW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    /// <summary>The OS message a titlebar X click sends — see <see cref="Actions.WindowClose"/>.</summary>
    public const uint WM_CLOSE = 0x0010;

    public const uint CF_UNICODETEXT = 13;

    [System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true)]
    [return: System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)]
    public static extern bool OpenClipboard(IntPtr hWndNewOwner);

    [System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true)]
    [return: System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)]
    public static extern bool CloseClipboard();

    [System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true)]
    public static extern IntPtr GetClipboardData(uint uFormat);

    [System.Runtime.InteropServices.DllImport("kernel32.dll", SetLastError = true)]
    public static extern IntPtr GlobalLock(IntPtr hMem);

    [System.Runtime.InteropServices.DllImport("kernel32.dll", SetLastError = true)]
    [return: System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)]
    public static extern bool GlobalUnlock(IntPtr hMem);

    /// <summary>
    /// Bypass Windows' SetForegroundWindow restrictions by attaching the
    /// caller's thread input queue to the CURRENT FOREGROUND window's thread
    /// input queue, calling SetForegroundWindow(hWnd), then detaching. This is
    /// the canonical workaround when the calling process isn't the foreground
    /// process (UIA-driven test runners hit this) — per the documented recipe,
    /// the attach target is whoever currently HOLDS the foreground-change
    /// privilege (GetForegroundWindow()'s thread), not the thread of the
    /// window being brought forward. Attaching to the target's own thread (the
    /// bug this replaced, 2026-09-03, row 162) grants nothing: the whole
    /// problem is that the target ISN'T foreground yet, so its thread has no
    /// privilege to lend. It "worked" only when the target happened to already
    /// be (or self-activate into) the real foreground, i.e. exactly the cases
    /// that didn't need the workaround.
    /// </summary>
    public static void ForceForeground(IntPtr hWnd)
    {
        if (hWnd == IntPtr.Zero) return;
        ShowWindow(hWnd, SW_RESTORE);
        var currentThread = GetCurrentThreadId();
        var foregroundWnd = GetForegroundWindow();
        var foregroundThread = foregroundWnd == IntPtr.Zero
            ? 0u
            : GetWindowThreadProcessId(foregroundWnd, out _);
        if (foregroundThread == 0 || foregroundThread == currentThread)
        {
            SetForegroundWindow(hWnd);
            return;
        }
        try
        {
            AttachThreadInput(currentThread, foregroundThread, true);
            SetForegroundWindow(hWnd);
        }
        finally
        {
            AttachThreadInput(currentThread, foregroundThread, false);
        }
    }
}
