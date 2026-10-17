using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_core;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The Profile's <b>private section</b> on another person's profile — the viewer's own
/// nickname, notes and labels on them (profile.md § The private section).
///
/// <para>Glue over the shared <c>FfiOverlayEditor</c>: the staging (untouched fields
/// follow the live overlay, the first edit snapshots the baseline), the changed-register
/// diff, the bounds and every refusal text are shared Rust. This class owns the text the
/// fields show and nothing else — the windows twin of apple's <c>ProfilePrivateVM</c> and
/// android's <c>ProfilePrivateVM</c>. Every gesture only stages; nothing is written until
/// <see cref="SaveAsync"/>.</para>
///
/// <para>One editor per open of one person's profile (<see cref="Open"/>), so nothing
/// staged for the last person carries over. No <c>ConfigureAwait(false)</c> on the Save
/// await: the bound-state mutation after it must resume on the UI thread (the WinUI VM
/// rule).</para>
/// </summary>
public sealed class ProfilePrivateViewModel
{
    private FfiOverlayEditor? _editor;
    private OverlayForm _form = new("", "", Array.Empty<string>());

    /// <summary>What the nickname field shows (the staged edit, else the live overlay).</summary>
    public string Nickname => _form.nickname;

    /// <summary>What the notes field shows.</summary>
    public string Notes => _form.notes;

    /// <summary>The labels as display spellings, in the order shown — one removable
    /// chip each.</summary>
    public IReadOnlyList<string> Labels => _form.labels;

    /// <summary>The section's refusal or failure, for the page's <c>error-message</c>;
    /// null clears it. Resolved from the shared <c>LocalizedText</c>, never worded here.</summary>
    public string? ErrorMessage { get; private set; }

    /// <summary>A Save is in flight (the save button disables).</summary>
    public bool IsSaving { get; private set; }

    /// <summary>Begin a fresh open of one person's profile: a new editor over the
    /// manager as it stands now, and a clean error.</summary>
    public void Open(string actorId)
    {
        using var names = ContactOverlayNames.Open();
        _editor = names.Editor(actorId);
        ErrorMessage = null;
        IsSaving = false;
        Reload();
    }

    /// <summary>Drop the editor with the person it was for.</summary>
    public void Reset()
    {
        _editor = null;
        ErrorMessage = null;
        IsSaving = false;
        _form = new("", "", Array.Empty<string>());
    }

    /// <summary>Re-read the form: an untouched section follows a sibling device's
    /// edit. Returns whether what the section shows changed.</summary>
    public bool Reload()
    {
        if (_editor is null) return false;
        var live = _editor.Form();
        var changed = live.nickname != _form.nickname
            || live.notes != _form.notes
            || !live.labels.AsSpan().SequenceEqual(_form.labels);
        _form = live;
        return changed;
    }

    public void SetNickname(string value)
    {
        _editor?.SetNickname(value);
        Reload();
    }

    public void SetNotes(string value)
    {
        _editor?.SetNotes(value);
        Reload();
    }

    /// <summary>Stage the typed label. True when it was staged (the add field then
    /// empties); false when it was refused (the refusal shows, the text stays).</summary>
    public bool AddLabel(string raw)
    {
        if (_editor is null) return false;
        var refusal = _editor.AddLabel(raw);
        ErrorMessage = refusal is null ? null : Strings.Resolve(refusal);
        Reload();
        return refusal is null;
    }

    public void RemoveLabel(int index)
    {
        if (index < 0) return;
        _editor?.RemoveLabel((uint)index);
        Reload();
    }

    /// <summary>One Save for everything staged. A refusal keeps the staged edits on
    /// screen.</summary>
    public async Task SaveAsync()
    {
        var editor = _editor;
        if (editor is null || IsSaving) return;
        IsSaving = true;
        var outcome = await editor.Save();
        // The page moved to another person while this was in flight: the outcome is
        // about an editor this section no longer shows. (An editor is never compared
        // by reference elsewhere, but here the field holds the very wrapper this call
        // started from, so reference equality is sound.)
        if (!ReferenceEquals(_editor, editor)) return;
        ErrorMessage = outcome switch
        {
            FfiOverlaySave.Saved => null,
            FfiOverlaySave.Refused r => Strings.Resolve(r.text),
            FfiOverlaySave.NotReady n => Strings.Resolve(n.text),
            FfiOverlaySave.Failed f => Strings.Resolve(f.text),
            _ => null,
        };
        IsSaving = false;
        Reload();
    }
}
