using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The retired identity's own MLS store, for
/// <c>succession_retry_group_sweep</c>'s <c>old_store_path</c>
/// (<c>docs/goal/ui/settings.md</c> § Recovery kit → <i>Finishing an unfinished
/// group sweep</i>) — the retry's OTHER resolver, and deliberately not
/// <see cref="SuccessorStorePath"/>. The windows twin of apple's
/// <c>FaunaKit/Core/SuccessorStorePath.swift</c>'s <c>RetiredIdentityStorePath</c>.
///
/// <para>⚠ <b>Must be the PURE scope resolution, never the creating one.</b> The
/// retry turns on whether the retired identity's store already exists on this
/// device; <see cref="SuccessorStorePath.MlsDbPath"/> creates the directory as a side
/// effect of asking, which would make an untouched retired identity read as a
/// clean empty run while the thief's leaf sits untouched in every real group
/// (<c>libs/fauna-ffi/src/recovery.rs</c>'s warning on the same parameter).</para>
/// </summary>
internal sealed class RetiredIdentityStorePath : FfiSuccessorStorePath
{
    /// <summary>
    /// Resolve the retired identity's MLS store path — reads only, never creates
    /// the directory. A malformed hex resolves under
    /// <see cref="AccountStateDir.PureMlsDbPath"/>'s unresolved component, a path
    /// that never holds another account's live <c>mls.db</c>.
    /// </summary>
    public string MlsDbPath(string successorActorHex) =>
        AccountStateDir.PureMlsDbPath(successorActorHex);
}
