import FaunaFFISwift
import Foundation

#if DEBUG && canImport(FileProvider)
    import CryptoKit
    import FileProvider

    /// The hidden File Provider test-arg path — the headless driver behind the
    /// signing-gated tier_3 read-path proof
    /// (`tests/e2e-unified/tests/platform/macos/test_file_provider_read_path.py`)
    /// and any scripted domain check. Lifted verbatim-in-behavior from the M0
    /// packaging host (`Fauna-FileProvider-M0Host/`, deleted at slice 3b): domain
    /// registration must run from the appex-embedding app's own process
    /// (`NSFileProviderManager` binds domains to the calling bundle's extension),
    /// which is why this hangs off the app binary rather than a separate tool.
    ///
    /// `run` returns `nil` for argv it does not own (normal GUI launch — including
    /// zero args), else the process exit code. Only the ten exact FP verbs are
    /// intercepted, so ordinary app argv (URLs, Finder flags) falls through.
    ///
    /// **DEBUG-only, and it must be** (convention 15,
    /// `e2e-automation-surface-gating.md` § The convention): `provision` writes a
    /// nest URL, bearer and backup key from argv into the File Provider credential
    /// store, and `revoke`/`register`/`remove`/`signal`/`evict`/`read`/`write` act on the user's domain, so
    /// in a Release build any process running as the user could repoint or revoke
    /// that user's File Provider through the signed binary. There is deliberately NO
    /// `#else` twin: the Release build receives nothing from this file, and the one
    /// caller (`PlatformMainEntry`) gates its own call. It compiles on iOS too
    /// (`canImport(FileProvider)` holds there), so both apple apps get the same gate.
    public enum FileProviderTestCLI {
        public static func run(arguments: [String]) -> Int32? {
            guard let mode = arguments.first else { return nil }
            switch mode {
            case "provision", "revoke":
                return credentialCommand(arguments)
            case "register", "remove", "list":
                return domainCommand(
                    mode,
                    arguments.count > 1 ? arguments[1] : nil,
                    arguments.count > 2 ? arguments[2] : nil,
                    alwaysEnabled: arguments.count > 3 && arguments[3] == "always-enabled")
            case "signal":
                return signalCommand(
                    arguments.count > 1 ? arguments[1] : nil,
                    arguments.count > 2 ? arguments[2] : nil)
            case "evict":
                return evictCommand(Array(arguments.dropFirst()))
            case "read":
                return readCommand(Array(arguments.dropFirst()))
            case "write":
                return writeCommand(Array(arguments.dropFirst()))
            case "staged":
                return stagedCommand(Array(arguments.dropFirst()))
            default:
                return nil
            }
        }

        private static func stderrLine(_ message: String) {
            FileHandle.standardError.write(Data((message + "\n").utf8))
        }

        /// Decode an exactly-`expectedBytes`-long hex string (`nil` = any non-empty
        /// length); nil on malformed input so the caller fails the command rather
        /// than provision garbage key material.
        private static func hexToData(_ hex: String, expectedBytes: Int?) -> Data? {
            let chars = Array(hex)
            guard !chars.isEmpty, chars.count % 2 == 0,
                expectedBytes.map({ chars.count == $0 * 2 }) ?? true
            else { return nil }
            var data = Data(capacity: chars.count / 2)
            var i = 0
            while i < chars.count {
                guard let byte = UInt8(String(chars[i ... i + 1]), radix: 16) else { return nil }
                data.append(byte)
                i += 2
            }
            return data
        }

        private static func credentialCommand(_ args: [String]) -> Int32 {
            switch args[0] {
            case "provision":
                // provision <nest_url> <actor_id_hex> <device_id_hex> <device_label> <backup_key_hex> <bearer>
                //           [<writer_secret_hex> <device_authorization_hex>]
                guard args.count == 7 || args.count == 9 else {
                    stderrLine(
                        "usage: Fauna provision <nest_url> <actor_id_hex(32B)> <device_id_hex(32B)> "
                            + "<device_label> <backup_key_hex(32B)> <bearer> "
                            + "[<writer_secret_hex(32B)> <device_authorization_hex>]")
                    return 2
                }
                var signer: FfiChangeSignerCarriage?
                if args.count == 9 {
                    guard let secret = hexToData(args[7], expectedBytes: 32),
                        let authorization = hexToData(args[8], expectedBytes: nil)
                    else {
                        stderrLine(
                            "provision FAILED: writer_secret must be 32-byte hex, device_authorization hex")
                        return 1
                    }
                    signer = FfiChangeSignerCarriage(
                        writerSecret: secret, deviceAuthorization: authorization)
                }
                guard
                    let actorId = hexToData(args[2], expectedBytes: 32),
                    let deviceId = hexToData(args[3], expectedBytes: 32),
                    let backupKey = hexToData(args[5], expectedBytes: 32)
                else {
                    stderrLine(
                        "provision FAILED: actor_id / device_id / backup_key must each be 32-byte hex")
                    return 1
                }
                FileProviderCredentialStore.provision(
                    FileProviderCredentials(
                        nestURL: args[1],
                        actorId: actorId,
                        deviceId: deviceId,
                        deviceLabel: args[4],
                        backupKey: backupKey
                    ),
                    bearer: args[6],
                    signer: signer
                )
                // Intra-process read-back guard: proves the write itself succeeded (the
                // cross-process appex rendezvous is what the end-to-end enumerate/hydrate
                // proof exercises).
                if FileProviderCredentialStore.load() == nil {
                    let probe = FileProviderCredentialStore.diagnoseAccessGroupRoundTrip()
                    stderrLine(
                        "provision FAILED: wrote credentials but read-back returned nil "
                            + "(access-group probe: SecItemAdd=\(probe.add), SecItemCopyMatching=\(probe.read))"
                    )
                    return 1
                }
                print("provision OK")
                return 0
            default:  // "revoke"
                FileProviderCredentialStore.revoke()
                print("revoke OK")
                return 0
            }
        }

        /// `register <ref> <name>` / `remove <ref>` take the set's BARE
        /// `FolderRef` wire string, exactly what the app's reconcile is handed,
        /// and register the domain under the same actor-scoped identity the
        /// reconcile would (`FileProviderDomainIdentity`, scoped to the
        /// PROVISIONED account — so `provision` runs first, and the appex the
        /// e2e drives serves the production identifier grammar, never a bare
        /// ref it refuses). A non-ref id (the M0 smoke's `fauna-m0-test`) is
        /// registered raw — the packaging smoke needs no set at all.
        ///
        /// `register <ref> <name> always-enabled` adds the domain with the
        /// `.alwaysEnabled` testing mode: the OS adds a fresh domain disabled
        /// until a person enables it in the Files app (iOS) or System Settings
        /// (macOS), and serves nothing until then (FP -2011), so a headless run
        /// needs the mode to launch the extension at all. The mode is honoured
        /// only for a build signed with the
        /// `com.apple.developer.fileprovider.testing-mode` entitlement.
        private static func domainCommand(
            _ mode: String, _ idArg: String?, _ displayNameArg: String?,
            alwaysEnabled: Bool
        ) -> Int32 {
            let id: String
            if let ref = idArg, folderRefIsValid(wire: ref) {
                guard let creds = FileProviderCredentialStore.load() else {
                    stderrLine(
                        "\(mode) FAILED: \(ref) is a set ref, and a set's domain is scoped to the provisioned account — run `provision` first"
                    )
                    return 1
                }
                guard
                    let identity = FileProviderDomainIdentity(
                        actorIdHex: data_to_hex(creds.actorId), folderId: ref)
                else {
                    stderrLine("\(mode) FAILED: could not scope \(ref) to the provisioned account")
                    return 1
                }
                id = identity.domainId
            } else {
                id = idArg ?? "fauna-m0-test"
            }
            let domain = NSFileProviderDomain(
                identifier: NSFileProviderDomainIdentifier(rawValue: id),
                displayName: displayNameArg ?? id
            )
            var exitCode: Int32 = 0
            let done = DispatchSemaphore(value: 0)

            switch mode {
            case "register":
                if alwaysEnabled { domain.testingModes = [.alwaysEnabled] }
                NSFileProviderManager.add(domain) { error in
                    if let error {
                        stderrLine("register FAILED: \(error)")
                        exitCode = 1
                    } else {
                        print("register OK: \(id)")
                    }
                    done.signal()
                }
            case "remove":
                NSFileProviderManager.remove(domain) { error in
                    if let error {
                        stderrLine("remove FAILED: \(error)")
                        exitCode = 1
                    } else {
                        print("remove OK: \(id)")
                    }
                    done.signal()
                }
            default:  // "list"
                NSFileProviderManager.getDomainsWithCompletionHandler { domains, error in
                    if let error {
                        stderrLine("list FAILED: \(error)")
                        exitCode = 1
                    } else {
                        print("domains: " + domains.map(\.identifier.rawValue).joined(separator: ","))
                        // The Files-app / System Settings enablement the OS holds
                        // per domain — the observable behind `fileproviderd`'s
                        // `(disabled,…)` and its -2011 "Sync is not enabled".
                        for domain in domains {
                            print("domain \(domain.identifier.rawValue) userEnabled=\(domain.userEnabled)")
                        }
                    }
                    done.signal()
                }
            }

            done.wait()
            return exitCode
        }

        /// `evict <set-ref> <set-name> <rel>` — ask the OS to evict one item of the
        /// set's domain (`NSFileProviderManager.evictItem`), the programmatic twin
        /// of the user's *Remove Download*: the headless probe for whether the OS
        /// lets a kept own record go (`on-demand-files-apple.md` § Apple File
        /// Provider binding, *An own record the nest took no bytes of is kept
        /// downloaded*). Prints the OS's answer either way; the exit code is 0
        /// when the OS accepted the eviction and 1 when it refused, so the caller
        /// reads the refusal as a measurement, not a crash. The domain is resolved
        /// exactly as `register` scopes it, so `provision` runs first.
        private static func evictCommand(_ args: [String]) -> Int32 {
            guard args.count == 3 else {
                stderrLine("usage: Fauna evict <set-ref> <set-name> <rel>")
                return 2
            }
            let (ref, setName, rel) = (args[0], args[1], args[2])
            guard let manager = setManager("evict", ref, setName) else { return 2 }
            var exitCode: Int32 = 0
            let done = DispatchSemaphore(value: 0)
            manager.evictItem(identifier: NSFileProviderItemIdentifier(rawValue: rel)) { error in
                if let error {
                    print("evict REFUSED: \(rel): \(error)")
                    exitCode = 1
                } else {
                    print("evict OK: \(rel)")
                }
                done.signal()
            }
            done.wait()
            return exitCode
        }

        /// The manager of `<set-ref>`'s domain, resolved exactly as `register`
        /// scopes it (so `provision` runs first); nil after printing why.
        private static func setManager(_ verb: String, _ ref: String, _ setName: String)
            -> NSFileProviderManager?
        {
            guard let creds = FileProviderCredentialStore.load(),
                let identity = FileProviderDomainIdentity(
                    actorIdHex: data_to_hex(creds.actorId), folderId: ref)
            else {
                stderrLine(
                    "\(verb) FAILED: a set's domain is scoped to the provisioned account — run `provision` first"
                )
                return nil
            }
            let domain = NSFileProviderDomain(
                identifier: NSFileProviderDomainIdentifier(rawValue: identity.domainId),
                displayName: setName)
            guard let manager = NSFileProviderManager(for: domain) else {
                stderrLine("\(verb) FAILED: no manager for \(identity.domainId)")
                return nil
            }
            return manager
        }

        /// `read <set-ref> <set-name> <rel>` — open one file of the set's domain
        /// the way a person's app does: list the domain root (which drives the
        /// OS to enumerate), then a plain read of `<root>/<rel>`, which hydrates
        /// a dataless item through `fetchContents`. Prints whether the item was
        /// dataless before the read (`SF_DATALESS`), the byte count and the
        /// SHA-256 of what came back. On the iOS simulator the app is the only
        /// process that can read a domain's files (`simctl spawn` runs no
        /// `cat`), so this is the eviction probe's hydrate and its re-open
        /// (`on-demand-files-apple.md` § Apple File Provider binding). Exit 0 on
        /// a read, 1 when the item never listed or the read failed.
        private static func readCommand(_ args: [String]) -> Int32 {
            guard args.count == 3 else {
                stderrLine("usage: Fauna read <set-ref> <set-name> <rel>")
                return 2
            }
            let (ref, setName, rel) = (args[0], args[1], args[2])
            guard let manager = setManager("read", ref, setName) else { return 2 }
            guard let root = rootURL("read", manager) else { return 1 }
            let item = root.appendingPathComponent(rel)
            let deadline = Date().addingTimeInterval(30)
            while !FileManager.default.fileExists(atPath: item.path) {
                _ = try? FileManager.default.contentsOfDirectory(atPath: root.path)
                guard Date() < deadline else {
                    stderrLine("read FAILED: \(rel) never listed under \(root.path)")
                    return 1
                }
                Thread.sleep(forTimeInterval: 0.5)
            }
            var info = stat()
            let dataless = stat(item.path, &info) == 0 && (info.st_flags & UInt32(SF_DATALESS)) != 0
            do {
                let bytes = try Data(contentsOf: item)
                let digest = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
                print("read OK: \(rel) dataless-before=\(dataless) bytes=\(bytes.count) sha256=\(digest)")
                return 0
            } catch {
                print("read FAILED: \(rel) dataless-before=\(dataless): \(error)")
                return 1
            }
        }

        /// `staged <set-ref>` — list what the extension's engine **staging root**
        /// for `<set-ref>` holds (`fileProviderRootDir(for:)`, in the app-group
        /// container), one `staged: <rel> bytes=<n> sha256=<hex>` line per file,
        /// then `staged OK: <count>`. Read-only. The staging root is what the
        /// extension's own relay seat serves from (`relay-serving.md` § Relay
        /// serving → *Apple's File Provider replica*), so this is the observable
        /// a witness reads for "kept here" and "never fetched here"; it runs in
        /// the signed app's process, the one that may open the group container
        /// (a test process reading it raises the OS's other-app-data prompt).
        /// The domain is scoped exactly as `register` scopes it, so `provision`
        /// runs first. Exit 0 on a listing, 1 when the root cannot be read.
        private static func stagedCommand(_ args: [String]) -> Int32 {
            guard args.count == 1 else {
                stderrLine("usage: Fauna staged <set-ref>")
                return 2
            }
            let ref = args[0]
            guard let creds = FileProviderCredentialStore.load(),
                let identity = FileProviderDomainIdentity(
                    actorIdHex: data_to_hex(creds.actorId), folderId: ref)
            else {
                stderrLine(
                    "staged FAILED: a set's domain is scoped to the provisioned account — run `provision` first"
                )
                return 2
            }
            let root: URL
            do {
                root = try fileProviderRootDir(for: identity)
            } catch {
                stderrLine("staged FAILED: no staging root for \(identity.domainId): \(error)")
                return 1
            }
            guard
                let walker = FileManager.default.enumerator(
                    at: root, includingPropertiesForKeys: [.isRegularFileKey])
            else {
                stderrLine("staged FAILED: cannot list \(root.path)")
                return 1
            }
            var count = 0
            let prefix = root.standardizedFileURL.path + "/"
            for case let url as URL in walker {
                guard (try? url.resourceValues(forKeys: [.isRegularFileKey]))?.isRegularFile == true
                else { continue }
                let path = url.standardizedFileURL.path
                let rel = path.hasPrefix(prefix) ? String(path.dropFirst(prefix.count)) : path
                guard let bytes = try? Data(contentsOf: url) else {
                    print("staged: \(rel) unreadable")
                    continue
                }
                let digest = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
                print("staged: \(rel) bytes=\(bytes.count) sha256=\(digest)")
                count += 1
            }
            print("staged OK: \(count)")
            return 0
        }

        /// The user-visible root of `manager`'s domain; nil after printing why.
        private static func rootURL(_ verb: String, _ manager: NSFileProviderManager) -> URL? {
            var root: URL?
            let done = DispatchSemaphore(value: 0)
            manager.getUserVisibleURL(for: .rootContainer) { url, error in
                if let error { stderrLine("\(verb) FAILED: no root URL: \(error)") }
                root = url
                done.signal()
            }
            done.wait()
            return root
        }

        /// `write <set-ref> <set-name> <rel> <text>` — create one file in the set's
        /// domain the way a person's app does: a coordinated write of `<text>`
        /// (UTF-8) to `<root>/<rel>`, which the OS hands the extension as
        /// `createItem` — ingest, then a record signed by the provisioned
        /// principal. The own-record leg of the eviction probe
        /// (`on-demand-files-apple.md` § Apple File Provider binding): on the iOS
        /// simulator the app is the only process that can write into a domain.
        /// Returns once the bytes are on disk; the record lands asynchronously,
        /// so the caller waits on the nest for it. Exit 0 on a write, 1 on failure.
        private static func writeCommand(_ args: [String]) -> Int32 {
            guard args.count == 4 else {
                stderrLine("usage: Fauna write <set-ref> <set-name> <rel> <text>")
                return 2
            }
            let (ref, setName, rel, text) = (args[0], args[1], args[2], args[3])
            guard let manager = setManager("write", ref, setName) else { return 2 }
            guard let root = rootURL("write", manager) else { return 1 }
            // Listing the root drives the OS to enumerate the domain first.
            _ = try? FileManager.default.contentsOfDirectory(atPath: root.path)
            let item = root.appendingPathComponent(rel)
            let bytes = Data(text.utf8)
            var coordinationError: NSError?
            var writeError: Error?
            NSFileCoordinator().coordinate(
                writingItemAt: item, options: .forReplacing, error: &coordinationError
            ) { url in
                do { try bytes.write(to: url) } catch { writeError = error }
            }
            if let error = coordinationError ?? writeError.map({ $0 as NSError }) {
                print("write FAILED: \(rel): \(error)")
                return 1
            }
            let digest = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
            print("write OK: \(rel) bytes=\(bytes.count) sha256=\(digest)")
            return 0
        }

        /// Drive the nudge `FaunaClient.swift`'s push observer runs on a
        /// `fauna.sync.changed` push naming `<set-name>`: the push matched to the
        /// held set `<set-ref>` (`register <set-ref> <set-name>`'s set) through
        /// the same shared rule (`FileProviderDomains.domainIds`, by the set's own
        /// name — never the domain's display name), then that set's domain
        /// signalled by its identifier — the headless stand-in for the live push
        /// arm, so the remote-change-nudge round trip (`enumerateChanges` ->
        /// `host.refresh()` -> re-signal iff changed) is provable without a real
        /// WS-RPC push subscription (`file-sync.md` § Remote-change nudge).
        /// Signalling no domain is a failure here: the proof needs the nudge to
        /// have reached one.
        private static func signalCommand(_ ref: String?, _ setName: String?) -> Int32 {
            guard let ref, let setName else {
                stderrLine("usage: Fauna signal <set-ref> <set-name>")
                return 2
            }
            guard let creds = FileProviderCredentialStore.load() else {
                stderrLine(
                    "signal FAILED: a set's domain is scoped to the provisioned account — run `provision` first"
                )
                return 1
            }
            let held = [
                PresenceSet(
                    name: setName, setName: setName, folderId: ref, thisDeviceAccepts: true,
                    role: .own)
            ]
            let ids = FileProviderDomains.domainIds(
                namedBy: setName, folderHash: nil, in: held,
                actorHex: data_to_hex(creds.actorId))
            var exitCode: Int32 = 0
            let done = DispatchSemaphore(value: 0)
            Task {
                do {
                    let signalled = try await FileProviderDomains.signal(domainIds: ids)
                    if signalled == 0 {
                        stderrLine("signal FAILED: no registered domain for \(ref) (\(setName))")
                        exitCode = 1
                    } else {
                        print("signal OK: \(setName) (\(signalled) domain(s))")
                    }
                } catch {
                    stderrLine("signal FAILED: \(error)")
                    exitCode = 1
                }
                done.signal()
            }
            done.wait()
            return exitCode
        }
    }
#endif
