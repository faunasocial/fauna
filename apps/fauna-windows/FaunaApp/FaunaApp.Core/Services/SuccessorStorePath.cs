using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Windows' <c>db_path_for</c> — the per-account MLS store resolver the
/// succession ceremony invokes for the <b>successor</b> identity
/// (<c>docs/goal/behavior/identity-succession.md</c> § Implementation status
/// today). The windows twin of apple's
/// <c>FaunaKit/Core/SuccessorStorePath.swift</c>, over the same
/// <see cref="AccountStateDir"/> layout every other windows caller resolves
/// through.
///
/// <para><b>Why this is a callback and not a string.</b> The ceremony takes a
/// <i>resolver</i>, never an eager path, and that is load-bearing rather than
/// stylistic: resolving an account's directory here <b>writes</b> —
/// <see cref="AccountStateDir.MlsDbPath"/> creates the scoped directory. The pinned
/// ordering is that <i>an unreachable nest must fail before anything is
/// written</i>, so the shared ceremony calls this only <b>after</b> the
/// successor's <c>connect()</c> succeeds. Handing it a pre-computed path would
/// run that write unconditionally, including on the arm where the succession
/// never lands — which is exactly the shape
/// <c>libs/fauna-ffi/src/recovery.rs</c> makes unrepresentable by taking an
/// <see cref="FfiSuccessorStorePath"/> instead of a string.</para>
///
/// <para><b>Why the successor's store is a different file.</b> Both engines are
/// live at once during the post-succession group sweep — the old identity's, to
/// remove its leaf, and the successor's, to add it — and MLS holds one engine per
/// <c>mls.db</c>. <see cref="AccountStateDir.MlsDbPath"/> is per-actor
/// (<c>&lt;base&gt;\&lt;actor&gt;\mls.db</c>), so a successor whose actor id
/// differs from the predecessor's necessarily lands on a different file. Nothing
/// here has to enforce that; it follows from the layout.</para>
/// </summary>
internal sealed class SuccessorStorePath : FfiSuccessorStorePath
{
    /// <summary>
    /// Resolve (creating the directory as the layout requires) the successor's MLS
    /// store path.
    ///
    /// <para>Never throws and never returns an empty string: a malformed hex resolves
    /// under the unresolved component exactly as every other
    /// <see cref="AccountStateDir"/> caller does, because a ceremony that has
    /// already re-pointed the account must not fail over a directory name. This
    /// runs on a Rust-owned callback thread, so an escaping exception would cross
    /// the FFI boundary rather than land anywhere a user could see it.</para>
    /// </summary>
    public string MlsDbPath(string successorActorHex) =>
        AccountStateDir.MlsDbPath(successorActorHex);
}
