import FaunaFFISwift
import Foundation

#if DEBUG && canImport(FileProvider)
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
    /// zero args), else the process exit code. Only the six exact FP verbs are
    /// intercepted, so ordinary app argv (URLs, Finder flags) falls through.
    ///
    /// **DEBUG-only, and it must be** (convention 15,
    /// `e2e-automation-surface-gating.md` § The convention): `provision` writes a
    /// nest URL, bearer and backup key from argv into the File Provider credential
    /// store, and `revoke`/`register`/`remove`/`signal` act on the user's domain, so
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
                    arguments.count > 2 ? arguments[2] : nil)
            case "signal":
                return signalCommand(
                    arguments.count > 1 ? arguments[1] : nil,
                    arguments.count > 2 ? arguments[2] : nil)
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
        private static func domainCommand(
            _ mode: String, _ idArg: String?, _ displayNameArg: String?
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
                    }
                    done.signal()
                }
            }

            done.wait()
            return exitCode
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
