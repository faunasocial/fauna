using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using uniffi.fauna_ffi;
using FaunaApp.Core.Services;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Core.ViewModels;

/// <summary>One actor offered by the apex picker (<c>fauna.admin.users.list</c>): the
/// 32-byte actor id + its display label. Public so the host page (which loads the list
/// off <c>FfiNestClient.Admin().UsersList</c>) and the unit test can supply it across the
/// FaunaApp.Core assembly boundary.</summary>
public sealed record ApexActorOption(byte[] Id, string Label);

/// <summary>
/// The admin <c>admin-web</c> page (web-content-hosting.md § Admin apex hosting; ui.yaml
/// <c>admin-web</c>): designate which actor's <c>web</c> content serves the deployment apex
/// (<c>https://&lt;domain&gt;/</c>), exactly as the admin designates a per-domain catch-all
/// mail actor. A dumb projection over the shared <c>fauna_client_web::WebClient</c> through
/// its UniFFI <see cref="IFfiWebClient"/> seam (fake it for a deterministic unit test); the
/// actor list is supplied by an injected fetch (the page wires it to
/// <c>FfiNestClient.Admin().UsersList</c>, the test a canned lambda). Mirrors the
/// AdminDnsPage catch-all / role-address picker's option/index/trailing-entry shape and the
/// linux admin_web.rs reference. "None" (index 0) clears → the built-in info page.
/// </summary>
public partial class AdminWebViewModel : ObservableObject
{
    private readonly IFfiWebClient _web;
    private readonly Func<Task<IReadOnlyList<ApexActorOption>>> _loadActors;
    private readonly string _domain;

    // Parallel id map for ApexOptions: null at index 0 (None / clear), the actor id at
    // each other index. SelectApexAsync dispatches set_apex_actor with this id.
    private List<byte[]?> _apexIds = new() { null };

    /// <summary><c>admin-web-apex-actor-select</c> options. Index 0 = "None" (clear → info
    /// page); each other index an actor label. Bound to the ComboBox ItemsSource.</summary>
    [ObservableProperty] private IReadOnlyList<string> _apexOptions = new List<string>();

    /// <summary>The currently-designated option's index (0 = None). Bound to the ComboBox
    /// SelectedIndex; the page handler ignores a SelectionChanged that matches it (the
    /// render-time echo).</summary>
    [ObservableProperty] private int _apexSelectedIndex;

    /// <summary><c>admin-web-apex-info</c> — the apex URL explainer (<c>https://&lt;domain&gt;/</c>).</summary>
    [ObservableProperty] private string _apexInfoText = string.Empty;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;

    internal AdminWebViewModel(
        IFfiWebClient web,
        Func<Task<IReadOnlyList<ApexActorOption>>> loadActors,
        string domain)
    {
        _web = web;
        _loadActors = loadActors;
        _domain = domain;
    }

    /// <summary>Hydrate from <c>get_apex_actor</c> (the transport already tolerates the
    /// post-login connect race for a single RPC — transport.md § Request lifecycle
    /// step 3, no app-level retry needed) + the injected actor list, then build the
    /// picker. The apex-info line uses the shared <c>fauna_core::web::apex_url</c>
    /// projection.</summary>
    public async Task LoadAsync()
    {
        IsLoading = true;
        Error = null;
        try
        {
            var current = await _web.GetApexActor();
            IReadOnlyList<ApexActorOption> actors;
            try { actors = await _loadActors(); }
            catch (Exception) { actors = Array.Empty<ApexActorOption>(); }
            BuildPicker(current, actors);
            ApexInfoText = S.Format("admin/web_page/apex_info", FaunaFfiMethods.WebApexUrl(_domain));
        }
        catch (Exception ex)
        {
            Error = S.Error(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>Designate (index &gt; 0) or clear ("None", index 0) the apex actor
    /// (<c>set_apex_actor</c>), then re-hydrate so the selection reflects the persisted
    /// designation (mirrors linux set_apex → hydrate).</summary>
    public async Task SelectApexAsync(int index)
    {
        if (index < 0 || index >= _apexIds.Count) return;
        try
        {
            await _web.SetApexActor(_apexIds[index]);
            await LoadAsync();
        }
        catch (Exception ex)
        {
            Error = S.Error(ex);
        }
    }

    /// <summary>Build the picker options/ids/selected index: index 0 = "None" (clear → null
    /// actor id), then one entry per actor. A current designation not among the loaded
    /// actors (paginated out) gets a trailing "actor xxxx…" entry keeping it visible +
    /// selected rather than silently clearing it. Mirrors AdminDnsPage.BuildActorPicker.</summary>
    private void BuildPicker(byte[]? current, IReadOnlyList<ApexActorOption> actors)
    {
        var options = new List<string> { S.Get("admin/web_page/apex_none") };
        var ids = new List<byte[]?> { null };
        foreach (var a in actors)
        {
            options.Add(a.Label);
            ids.Add(a.Id);
        }
        var selected = 0;
        if (current is { } cur)
        {
            var found = -1;
            for (var i = 0; i < actors.Count; i++)
            {
                if (actors[i].Id.AsSpan().SequenceEqual(cur)) { found = i; break; }
            }
            if (found >= 0)
            {
                selected = found + 1;
            }
            else
            {
                options.Add(AdminActorOptions.NotLoadedFallbackLabel(cur));
                ids.Add(cur);
                selected = options.Count - 1;
            }
        }
        _apexIds = ids;
        ApexOptions = options;
        ApexSelectedIndex = selected;
    }
}
