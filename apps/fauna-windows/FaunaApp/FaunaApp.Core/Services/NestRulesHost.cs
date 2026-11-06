using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using uniffi.fauna_client_policy;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The member's side of the nest rules on windows (<c>nest-rules.md</c> § The nest
/// content policy, § Acceptance at joining, and a change of rules) — a paint of the
/// shared <c>FfiNestRulesBook</c> (<c>libs/fauna-ffi/src/nest_rules.rs</c> over
/// <c>fauna_client_policy::NestRulesBook</c>). android's <c>core/NestRulesHost.kt</c>,
/// apple's <c>FaunaKit/Core/NestRulesStore.swift</c> and tui's
/// <c>apps/fauna-tui/src/nest_rules.rs</c> are the sibling legs.
///
/// <para>Everything that decides — verifying the nest's signature, keeping the
/// last-accepted document per nest, saying whether a read was a change, the rule
/// sets and keyword/labeler factors the engine folds, the summary the settings
/// surface paints — is shared Rust behind the book. What is left here is what the
/// design lets diverge: where the book's records live (<see cref="AccountStateDir.Base"/>,
/// install-scoped like the region record — a nest's signed document is public,
/// never a secret, so a change made while the app was closed is still found by its
/// newer sequence); when to read (every sign-in, every reconnect and every
/// <c>nest_info</c> push — <see cref="INestRpcClient.NestInfoStale"/>); and the
/// paint.</para>
///
/// <para>Process-lifetime: the book is a record per nest identity and stays across
/// sign-out; <see cref="ClearSession"/> forgets only which nest this session reads
/// through.</para>
/// </summary>
public static class NestRulesHost
{
    private const string FileName = "nest-rules-book.json";

    private static readonly object _gate = new();
    private static readonly SemaphoreSlim _reading = new(1, 1);
    private static FfiNestRulesBook? _book;
    private static string? _record;
    private static string? _sessionNestId;
    private static string? _sessionDomain;
    private static string? _changedNotice;

    /// <summary>Raised (on whatever thread the read landed on) whenever what the
    /// book answers may have changed — a read folded, a session cleared. Every
    /// surface that composed a verdict, painted the settings rules surface or the
    /// changed notice re-reads.</summary>
    public static event Action? Changed;

    /// <summary>
    /// Open the book at launch, the persisted records restored (each verified again
    /// in shared Rust) AHEAD of the first read. Idempotent. An unreadable record
    /// starts the book empty; the first read fills it.
    /// </summary>
    internal static void Open()
    {
        lock (_gate)
        {
            if (_book is not null) return;
            _record = Path.Combine(AccountStateDir.Base, FileName);
            try
            {
                _book = FfiNestRulesBook.Restore(ReadRecords(_record));
            }
            catch (Exception ex)
            {
                ShellLog.Warn("NestRulesHost", $"stored book not restorable: {ex.GetType().Name}: {ex.Message}");
                _book = new FfiNestRulesBook();
            }
        }
        Changed?.Invoke();
    }

    /// The book, opened on first use when <see cref="Open"/> has not run (a unit
    /// test, a preview) — never persisted then.
    private static FfiNestRulesBook BookOrOpen()
    {
        lock (_gate)
        {
            return _book ??= new FfiNestRulesBook();
        }
    }

    /// <summary>
    /// Read the session nest's self-description past the connection's memo and fold
    /// it — at sign-in, on every reconnect and on every <c>nest_info</c> push.
    /// Best-effort: a failed read leaves the last-known rules in force and never
    /// throws into the caller; a change persists the book and raises the notice.
    /// </summary>
    internal static async Task RefreshAsync(INestRpcClient rpc)
    {
        await _reading.WaitAsync().ConfigureAwait(false);
        try
        {
            var book = BookOrOpen();
            var r = await rpc.RefreshNestRulesBookAsync(book).ConfigureAwait(false);
            lock (_gate)
            {
                _sessionNestId = string.IsNullOrEmpty(r.nestId) ? null : r.nestId;
                _sessionDomain = string.IsNullOrEmpty(r.domain) ? null : r.domain;
            }
            switch (r.read)
            {
                case FfiNestRulesRead.Changed changed:
                    lock (_gate) _changedNotice = changed.domain;
                    Persist(book);
                    break;
                case FfiNestRulesRead.First:
                    Persist(book);
                    break;
                case FfiNestRulesRead.Refused refused:
                    ShellLog.Warn("NestRulesHost", $"self-description refused, last-known rules stay: {refused}");
                    break;
            }
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestRulesHost", $"fauna.nest.info failed: {ex.GetType().Name}: {ex.Message}");
        }
        finally
        {
            _reading.Release();
            Changed?.Invoke();
        }
    }

    /// <summary>
    /// The session nest's domain as its self-description named it — the publisher
    /// the admin's editor writes under (<c>FfiRuleEditor.ForNest</c>). Read now,
    /// awaited, when no read has landed yet; <c>null</c> when it cannot be read.
    /// </summary>
    internal static async Task<string?> SessionDomainAsync(INestRpcClient rpc)
    {
        lock (_gate)
        {
            if (_sessionDomain is not null) return _sessionDomain;
        }
        await RefreshAsync(rpc).ConfigureAwait(false);
        lock (_gate) return _sessionDomain;
    }

    /// <summary>The departing session's nest goes on an identity change; the book
    /// (a record per nest identity, public) stays for the next sign-in.</summary>
    internal static void ClearSession()
    {
        lock (_gate)
        {
            _sessionNestId = null;
            _sessionDomain = null;
            _changedNotice = null;
        }
        Changed?.Invoke();
    }

    /// <summary>The changed notice's nest domain until the member opens the rules
    /// surface; <c>null</c> when there is none.</summary>
    public static string? ChangedNoticeDomain
    {
        get { lock (_gate) return _changedNotice; }
    }

    /// <summary>The changed notice's text (<c>nest_rules.changed_notice</c>), or
    /// <c>null</c> — also the additive <c>nest_rules_changed_notice</c> e2e state
    /// field (<c>tests/e2e-unified/tests/test_nest_rules.py</c>).</summary>
    public static string? ChangedNoticeText
    {
        get
        {
            var domain = ChangedNoticeDomain;
            return domain is null
                ? null
                : Strings.Resolve(new uniffi.fauna_core.LocalizedText(
                    "nest_rules.changed_notice", new Dictionary<string, string> { ["nest"] = domain }));
        }
    }

    /// <summary>The member opened the rules surface — the notice has done its job.</summary>
    public static void DismissNotice()
    {
        bool had;
        lock (_gate)
        {
            had = _changedNotice is not null;
            _changedNotice = null;
        }
        if (had) Changed?.Invoke();
    }

    /// <summary>The book and the session nest it binds, or <c>null</c> before both
    /// exist — what <see cref="RegionPlaneHost.Render"/> hands the composed call.</summary>
    internal static (FfiNestRulesBook Book, string NestId)? Source()
    {
        lock (_gate)
        {
            if (_book is null || _sessionNestId is null) return null;
            return (_book, _sessionNestId);
        }
    }

    /// <summary>What the settings rules surface paints for the session nest;
    /// <c>null</c> when it published none (or before its self-description was
    /// read). Whether the surface paints is the shared <c>is_shown</c> rule:
    /// <see cref="IsShown"/>.</summary>
    internal static NestRulesSummary? Summary()
    {
        var src = Source();
        if (src is null) return null;
        try
        {
            return src.Value.Book.Summary(src.Value.NestId, RegionPlaneHost.UiLang);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestRulesHost", $"summary unreadable: {ex.GetType().Name}: {ex.Message}");
            return null;
        }
    }

    /// <summary>The shared <c>NestRulesSummary::is_shown</c>: a surface paints a
    /// summary with a rule, or one none of which applies on this device (its
    /// sentence shown with no rule listed) — never an applied empty document.</summary>
    internal static bool IsShown(NestRulesSummary? s) =>
        s is not null && (s.rules.Length > 0 || s.notAppliedSentence is not null);

    /// <summary>One rule as both rules surfaces (joining and settings) list it: the
    /// verdict, the level where the rule has one, and the nest's own reason verbatim
    /// — an argument, never a template, so the shared single-pass fill leaves its
    /// braces alone (<c>nest_rules.rule_line</c> / <c>.rule_line_no_level</c>; tui's
    /// and linux's <c>nest_rules::rule_line</c>, android's <c>ruleLine</c>, apple's
    /// <c>nestRuleLineText</c>).</summary>
    internal static string RuleLine(NestRuleLine rule)
    {
        var args = new Dictionary<string, string>
        {
            ["verdict"] = Strings.Resolve(rule.verdict),
            ["reason"] = rule.reason,
        };
        if (rule.level is { } level)
        {
            args["level"] = Strings.Resolve(level);
            return Strings.Resolve(new uniffi.fauna_core.LocalizedText("nest_rules.rule_line", args));
        }
        return Strings.Resolve(new uniffi.fauna_core.LocalizedText("nest_rules.rule_line_no_level", args));
    }

    // ── Persistence: the book's signed records, as JSON under the install base ─

    private sealed record Row(
        string nest_id, string domain, string document, string sequence, long issued_at, string signature);

    private static void Persist(FfiNestRulesBook book)
    {
        string? path;
        lock (_gate) path = _record;
        if (path is null) return;
        try
        {
            var rows = book.Stored().Select(r => new Row(
                r.nestId, r.domain, Convert.ToBase64String(r.document), r.sequence.ToString(),
                r.issuedAt, Convert.ToBase64String(r.signature))).ToList();
            var json = JsonSerializer.Serialize(rows);
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            var tmp = path + ".tmp";
            File.WriteAllText(tmp, json);
            File.Move(tmp, path, overwrite: true);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestRulesHost", $"persist failed: {ex.GetType().Name}: {ex.Message}");
        }
    }

    private static StoredNestRules[] ReadRecords(string path)
    {
        if (!File.Exists(path)) return Array.Empty<StoredNestRules>();
        var rows = JsonSerializer.Deserialize<List<Row>>(File.ReadAllText(path)) ?? new List<Row>();
        return rows.Select(o => new StoredNestRules(
            o.nest_id, o.domain, Convert.FromBase64String(o.document), ulong.Parse(o.sequence),
            o.issued_at, Convert.FromBase64String(o.signature))).ToArray();
    }

    // ── Convention 17: "a nest Block never renders silent" ───────────────────
    //
    // The region's twin (RegionPlaneHost.WitnessBlocked): `blocked` holds the
    // on-screen items whose composed verdict a nest rule blocks, registered from the
    // verdict each surface computed; `painted` the nest block placeholders actually
    // realized. tui's, web's, android's and apple's `nest_block_render`. UI-thread
    // only, like the region sets.
    private static readonly HashSet<string> _blocked = new();
    private static readonly HashSet<string> _painted = new();

    /// Register (or clear) an on-screen item container's verdict-side walk.
    public static void WitnessBlocked(string key, bool blocked)
    {
        if (blocked) _blocked.Add(key); else _blocked.Remove(key);
    }

    /// Register (or clear) a realized nest BLOCK placeholder.
    public static void WitnessPainted(string key, bool painted)
    {
        if (painted) _painted.Add(key); else _painted.Remove(key);
    }

    /// Drop an item container's registrations when it leaves the screen.
    public static void Forget(string key)
    {
        _blocked.Remove(key);
        _painted.Remove(key);
    }

#if DEBUG || FAUNA_E2E_AGENT
    /// <summary>The <c>nest_block_render</c> state field
    /// (<c>tests/e2e-unified/helpers/frame_invariants.py</c>). Read only by the
    /// e2e state protocol, so it compiles out of Release with it (convention 15).</summary>
    public static Dictionary<string, object?> BlockRenderState() => new()
    {
        ["blocked"] = _blocked.Count,
        ["placeholders"] = _painted.Count,
    };
#endif
}
