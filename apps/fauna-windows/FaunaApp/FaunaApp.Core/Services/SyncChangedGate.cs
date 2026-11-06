using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Whether a <c>fauna.sync.changed</c> push names the set this page knows as
/// <c>name</c> (path-sealing.md § the set-name plane, <i>Outbound carriers</i>).
/// A sealed set's push arrives with a blank <c>folder</c> and only its
/// <c>folder_hash</c>, so the name is never compared directly: the one shared
/// predicate (<c>SyncChangedPayload::names_set</c>) decides.
/// </summary>
public static class SyncChangedGate
{
    public static bool NamesSet(string folder, byte[]? folderHash, string name) =>
        FaunaFfiMethods.SyncChangedNamesSet(folder, folderHash, name);
}
