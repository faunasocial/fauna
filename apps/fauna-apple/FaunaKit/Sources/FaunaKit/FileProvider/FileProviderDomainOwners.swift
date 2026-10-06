import Foundation

/// Which account owns each registered File Provider domain: a domain-identifier
/// (the set's actor-scoped identity, `FileProviderDomainIdentity`) →
/// actor-id-hex map at the flat **container** base
/// (`<app-group>/sync/domain-owners.json`) — a steward write on macOS, where the
/// container is the sandboxed extension's domain and the app reaches in only to
/// keep the extension's records (`SyncStateDir`'s two-domain doc).
///
/// Why it exists (`on-demand-files.md` § Multi-account × File Provider,
/// consequences 2 + 3): the iOS upload-drain gate can leave a dirty domain
/// **registered across a sign-out or account switch** (removal refuses while
/// un-recorded edits exist, so the OS retry can drain them), and the capability
/// store is deliberately single-slot — so a lingering domain's extension would
/// otherwise be served under the *incoming* account's credentials, ingesting
/// the outgoing account's queued bytes into the wrong nest. Since 2026-09-25
/// the identifier itself carries the account (*the actor-scoped device
/// identity*), which closes that structurally — two accounts' `local:1` are
/// two identifiers, and the extension refuses a domain scoped to another
/// account before it reads this map — so the record is **defense in depth**:
/// the app records the provisioned actor when it adds a domain and backfills
/// it at every reconcile only where no owner is on record, never over another
/// account's, and the extension
/// refuses to serve a domain whose recorded owner differs from its provisioned
/// credentials. The drain gate no longer consults it: the account it needs is
/// in the identifier.
///
/// App-side single writer (add / reconcile backfill / successful remove); the
/// extension only reads. A record for a set with no registered domain is
/// harmless (the next reconcile of its owner re-registers or the successful
/// removal clears it; another account's add supersedes it — no domain, so no
/// queued edits, is behind it). A registered domain can still lack a record —
/// a crash between the OS add and `record`, a swallowed write below, or an
/// unreadable file read as empty — so an absent record is served (the
/// identifier's actor scope is the structural guard), never wedged, and the
/// next reconcile backfills it.
public enum FileProviderDomainOwners {
    static let filename = "domain-owners.json"

    /// `public` only because it seeds the public API's default arguments (an
    /// internal symbol can't) — production callers never pass `at:`.
    public static func defaultURL() -> URL? {
        SyncStateDir.containerSyncDir()?.appendingPathComponent(filename)
    }

    /// The recorded owner (lowercase actor-id hex) of one domain, if any.
    public static func owner(domainId: String, at url: URL? = defaultURL()) -> String? {
        guard let url else { return nil }
        return load(url)[domainId]
    }

    /// The whole record (domain identifier → owner actor-id hex) — what the
    /// shared presence plan takes as its owner input.
    public static func all(at url: URL? = defaultURL()) -> [String: String] {
        guard let url else { return [:] }
        return load(url)
    }

    /// Record (upsert) one domain's owner. Called by the app after it adds a
    /// domain and for the reconcile's backfill of a registered domain with no
    /// owner on record — the shared presence plan decides which; the store
    /// itself never refuses a write.
    public static func record(domainId: String, actorIdHex: String, at url: URL? = defaultURL()) {
        guard let url else { return }
        var map = load(url)
        map[domainId] = actorIdHex.lowercased()
        save(map, to: url)
    }

    /// Forget one domain's owner — called only after its removal succeeded.
    public static func clear(domainId: String, at url: URL? = defaultURL()) {
        guard let url else { return }
        var map = load(url)
        guard map.removeValue(forKey: domainId) != nil else { return }
        save(map, to: url)
    }

    private static func load(_ url: URL) -> [String: String] {
        guard let data = try? Data(contentsOf: url) else { return [:] }
        return (try? JSONDecoder().decode([String: String].self, from: data)) ?? [:]
    }

    private static func save(_ map: [String: String], to url: URL) {
        try? FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        if let data = try? JSONEncoder().encode(map) {
            try? data.write(to: url, options: .atomic)
        }
    }
}
