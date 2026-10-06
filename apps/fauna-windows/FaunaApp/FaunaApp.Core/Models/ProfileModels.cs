using System.Collections.Generic;
using CommunityToolkit.Mvvm.ComponentModel;

namespace FaunaApp.Core.Models;

/// <summary>
/// One editable label + uri row of the profile edit form's links list
/// (<c>profile.md</c> § State &amp; data shape; the FFI mirror of
/// <c>fauna_core::data::ProfileLink</c>). An <see cref="ObservableObject"/> so the
/// XAML two-way-binds <see cref="Label"/> / <see cref="Uri"/> directly to the row
/// (the linux <c>edit.rs</c> per-row text inputs). The VM clones the fetched rows
/// into fresh instances so live edits don't mutate the fetched model.
/// </summary>
public partial class ProfileLinkRow : ObservableObject
{
    [ObservableProperty] private string _label;
    [ObservableProperty] private string _uri;

    public ProfileLinkRow(string label, string uri)
    {
        _label = label;
        _uri = uri;
    }
}

/// <summary>
/// The three editable display fields of a stored profile, projected from the
/// opaque profile body by <c>decode_profile_display</c> (the non-display fields —
/// avatar / banner / nests / admin_nests / load_hint / inbox_mode — are dropped
/// from the projection but preserved across an edit by <c>build_edited_profile</c>'s
/// read-modify-write, so the client never round-trips them). Mirrors linux
/// <c>edit.rs</c> / the WASM display projection.
/// </summary>
public sealed record ProfileDisplay(
    string? DisplayName,
    string? Bio,
    IReadOnlyList<ProfileLinkRow> Links);

/// <summary>
/// The result of a profile read for the edit form: the decoded
/// <see cref="Display"/> PLUS the opaque <see cref="RawBody"/> the read-modify-write
/// save (<c>build_edited_profile</c>) needs as its base so the non-display fields
/// survive the edit. A <c>null</c> result models an unpublished profile (the
/// first-publish path — <see cref="RawBody"/> is then <c>null</c> too).
/// </summary>
public sealed record ProfileGetResult(
    ProfileDisplay Display,
    byte[] RawBody);
