namespace FaunaApp.Core.Models;

/// <summary>
/// One row of the <c>file-version-history</c> component inside
/// <c>media-item-detail</c> (media.md § Element IDs) — a display projection of the
/// shared <c>FileVersionSummary</c> (UniFFI <c>uniffi.fauna_media_machine</c>) that
/// <c>MediaMachine::file_versions</c> returns, oldest→newest.
/// <para>
/// The summary record is <c>internal</c> to the generated bindings, so the page maps
/// each one to this public record for the <c>x:Bind</c> DataTemplate — the same
/// projection <see cref="MediaItem"/> performs for <c>media-item</c>. Both display
/// strings are formatted through <c>ValueFormat</c>, i.e. through shared Rust
/// (<c>fauna_core::format::{relative_time, byte_size}</c>), so a version row reads
/// identically on every app (priority #1/#2 — never hand-roll the buckets).
/// </para>
/// </summary>
/// <param name="VersionNum">Stable identity — the recording <c>sync_changes</c> row's
/// <c>seq</c>, never renumbered (file-sync.md § File Versions). Carried on the row's
/// restore <c>Button.Tag</c> so a click resolves back to the exact
/// <c>FileVersionSummary</c> to restore; the <em>display</em> ordinal is the row's list
/// position, never this number.</param>
/// <param name="Timestamp">The version's <c>created_at</c> rendered for
/// <c>file-version-timestamp</c> — a relative time ("5m ago"), absolute past 7 days.</param>
/// <param name="Size">The version's <c>size_bytes</c> rendered for
/// <c>file-version-size</c>.</param>
/// <param name="Author">"Edited by ‹handle›" for <c>file-version-author</c>.</param>
/// <param name="Pruned">This version is soft-pruned (file-versions.md §
/// Retention (3)) — only an <c>include_pruned</c> listing carries a
/// <c>true</c> row. Gates <c>file-version-pruned-badge</c> and
/// <c>file-version-undelete-button</c>, present only on pruned rows.</param>
public record FileVersionRow(
    long VersionNum,
    string Timestamp,
    string Size,
    string Author,
    bool Pruned
);
