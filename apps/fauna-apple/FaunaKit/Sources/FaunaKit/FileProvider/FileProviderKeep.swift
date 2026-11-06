#if os(macOS)
    import FileProvider

    /// The keep of a body the holder-keeps gate holds — this seat's own record,
    /// materialized, in a folder whose nest took no bytes (`relay-serving.md`
    /// § Relay serving → *A holder keeps what it wrote*). The host marks such an
    /// item `kept` (`FfiFileProviderItem.kept`); on macOS the item then declares
    /// the keep-downloaded content policy, which takes it out of the OS's own
    /// eviction — storage pressure and *Remove Download* — while it still reads as
    /// synced: the shape the cfapi pin gives the same body on windows
    /// (`on-demand-files-apple.md` § Apple File Provider binding). Every other item
    /// inherits the domain's policy and stays evictable.
    ///
    /// macOS only: the keep-downloaded policy does not exist on iOS, where a kept
    /// item instead leaves out the `.allowsEvicting` capability every other file
    /// advertises (`FileProviderItemCapabilities`).
    public enum FileProviderKeep {
        public static func contentPolicy(kept: Bool) -> NSFileProviderContentPolicy {
            kept ? .downloadEagerlyAndKeepDownloaded : .inherited
        }
    }
#endif
