import SwiftUI

@MainActor @Observable
public class NostrVM {
    public var status: NostrStatus?
    public var follows: [NostrFollow] = []
    /// Relay URLs parsed from `status.relayList` (the nest stores the
    /// `relay_list` setting as a JSON array of strings; empty/None → []). When
    /// empty the nest publishes to its `DEFAULT_RELAYS` fallback.
    public var relays: [String] = []
    public var isLoading = false
    public var errorMessage: String?

    /// Succession-aftermath npub confirm banner owed-check (`nostr.md` §
    /// Key succession and rotation, leg 3) — best-effort like the shared Rust
    /// function itself: any unhappy read degrades to `false` rather than an
    /// error, so it is simply re-asked on the next `refresh()`.
    public var npubConfirmationOwed = false

    // Link form state
    public var linkMode: LinkMode = .generate
    public var importNsec = ""
    public var bunkerUrl = ""

    // Relay form state
    public var relayInput = ""

    // Follow form state
    public var followPubkey = ""
    public var followPetname = ""

    // Connected apps (NIP-46 bunker) invite start — the one-time invite reveal
    // (nostr.md § The nest as the user's NIP-46 signer). The roster of
    // connections is the Connected apps page's (connected-apps.md), not this page's.
    public var bunkerInvite: FfiCreateBunkerInviteReply?

    // Zap signers (NIP-57 trust root) state — the designated-signer roster
    // (nostr.md § Layout & flow item 7; monetization.md § Zap receipts — the
    // trust model). Rendered for ANY linked account (unlike Connected apps).
    //
    // The whole surface excises with `payments` — `zaps` is a subset member of
    // it (ui.yaml's `gated_features:`), and apple carries one condition for the
    // family (APIClient's zap-signers section says why). Gating the STATE and
    // not only the API call is deliberate: an ungated inert roster would let
    // the render below compile, paint nothing, and still ship every
    // `nostr-zap-signer-*` id (dynamic-features.md § Platform-family surface
    // excision — the render hazard).
    #if !FAUNA_EXCISE_PAYMENTS
    public var zapSigners: [FfiZapSignerEntry] = []
    public var zapSignerPubkeyInput = ""
    public var zapSignerLabelInput = ""
    /// Why `nostr-zap-signer-add-btn` is dead, or `nil` when it works. `nil`
    /// until the gate fetch resolves, which is what keeps an un-hydrated read
    /// from disabling the button eagerly (mirrors linux's
    /// `zap_signer_add_gate_reason` exactly). `remove` is never gated.
    public var zapSignerAddGateReason: String?
    #endif

    /// The *Connected apps* section renders only for a linked account in a
    /// custodial signing mode — a `remote`/`nip07` account has no key on the
    /// box to sign with, so it can never itself be a bunker. Mirrors web's
    /// `showConnectedApps` / android's `CUSTODIAL_MODES` gate.
    public var showConnectedApps: Bool {
        guard status?.linked == true, let mode = status?.mode else { return false }
        return mode == "generated" || mode == "imported"
    }

    public enum LinkMode: String, CaseIterable, Identifiable {
        case generate
        case `import`
        case remote

        public var id: String { rawValue }
    }

    private var api: APIClient?

    public init() {}

    public func configure(api: APIClient) {
        self.api = api
    }

    public func refresh() async {
        guard let api else { return }
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }
        do {
            status = try await api.getNostrStatus()
            relays = NostrVM.parseRelayList(status?.relayList)
            if status?.linked == true {
                follows = try await api.listNostrFollows()
                npubConfirmationOwed = (try? await api.npubConfirmationOwed()) ?? false
                // Any linked account has a pubkey a receipt's `p` tag can
                // name — unlike Connected apps, this is NOT custodial-gated
                // (nostr.md § Layout & flow item 7).
                #if !FAUNA_EXCISE_PAYMENTS
                await refreshZapSigners()
                await refreshZapSignerGate()
                #endif
            } else {
                follows = []
                npubConfirmationOwed = false
                #if !FAUNA_EXCISE_PAYMENTS
                zapSigners = []
                zapSignerAddGateReason = nil
                #endif
            }
            // Only a custodial account can be a bunker; a revealed invite does
            // not outlive that (mirrors web's `refresh`).
            if !showConnectedApps {
                bunkerInvite = nil
            }
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    public func link() async {
        guard let api else { return }
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }
        do {
            switch linkMode {
            case .generate:
                _ = try await api.linkNostrGenerate()
            case .import:
                guard !importNsec.isEmpty else {
                    errorMessage = L.errors.nostrNsecRequired
                    return
                }
                _ = try await api.linkNostrImport(nsec: importNsec)
                importNsec = ""
            case .remote:
                guard !bunkerUrl.isEmpty else {
                    errorMessage = L.errors.nostrBunkerRequired
                    return
                }
                _ = try await api.linkNostrRemote(bunkerUrl: bunkerUrl)
                bunkerUrl = ""
            }
            await refresh()
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    public func unlink() async {
        guard let api else { return }
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }
        do {
            try await api.unlinkNostr()
            await refresh()
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    /// "Yes, that's my npub" — the banner disappears because the fresh
    /// `refresh()` read says the confirmation is no longer owed (non-optimistic,
    /// like every other mutation on this page).
    public func confirmNpub() async {
        guard let api else { return }
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }
        do {
            try await api.confirmNpub()
            await refresh()
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    /// "No / nothing is linked" — routes into the *existing* new-key path
    /// rather than a bespoke one (`nostr.md`:75 — "the remedy is the existing
    /// page machinery"): unlinking drops the wrong (or thief-relinked) key and
    /// reveals the link form, where the owner generates or imports a fresh one
    /// exactly as any first-time link would.
    public func dismissNpubToNewKey() async {
        await unlink()
    }

    // MARK: - Relays

    /// Add a relay URL to the list and persist. The nest re-parses the JSON
    /// array and republishes NIP-65; a relay-only `set_settings` is a partial
    /// update (leaves the content toggles untouched — `db::update_settings`).
    public func addRelay() async {
        guard let url = trimmedRelayInput(input: relayInput) else { return }
        if let refusal = relayUrlError(url: url) {
            errorMessage = renderLocalizedText(refusal)
            return
        }
        errorMessage = nil
        guard let next = relayListAppending(existing: relays, url: url) else {
            relayInput = ""
            return
        }
        await writeRelays(next)
        relayInput = ""
    }

    public func removeRelay(url: String) async {
        await writeRelays(relays.filter { $0 != url })
    }

    private func writeRelays(_ list: [String]) async {
        guard let api else { return }
        do {
            try await api.updateNostrSettings(NostrSettings(relayList: NostrVM.encodeRelayList(list)))
            await refresh()
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    /// Parse the nest's `relay_list` setting (a JSON array of strings; `""`/nil
    /// when the account has no explicit list) into relay URLs.
    nonisolated static func parseRelayList(_ json: String?) -> [String] {
        guard let json,
              !json.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              let data = json.data(using: .utf8),
              let list = try? JSONDecoder().decode([String].self, from: data)
        else { return [] }
        return list
    }

    /// Encode relay URLs back to the JSON-array string the nest stores.
    nonisolated static func encodeRelayList(_ list: [String]) -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .withoutEscapingSlashes
        guard let data = try? encoder.encode(list),
              let json = String(data: data, encoding: .utf8)
        else { return "[]" }
        return json
    }

    /// Write one boolean bridge setting by its **wire key** — the key the
    /// shared content-toggle catalog handed the view
    /// (`fauna_ffi::bridges::nostr_content_toggle_options`, `nostr.md`
    /// § Where logic lives). Replaces the five named arguments, which were the
    /// same five-row table a third time (view + model + here).
    public func updateSetting(key: String, value: Bool) async {
        guard let api else { return }
        let settings = NostrSettings(flags: [key: value])
        do {
            try await api.updateNostrSettings(settings)
            await refresh()
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    public func addFollow() async {
        guard let api else { return }
        let pubkey = followPubkey.trimmingCharacters(in: .whitespaces)
        guard !pubkey.isEmpty else { return }
        errorMessage = nil
        do {
            let petname = followPetname.trimmingCharacters(in: .whitespaces)
            try await api.addNostrFollow(
                pubkey: pubkey,
                petname: petname.isEmpty ? nil : petname,
                relayHints: nil
            )
            followPubkey = ""
            followPetname = ""
            await refresh()
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    public func removeFollow(npub: String) async {
        guard let api else { return }
        do {
            try await api.removeNostrFollow(npub: npub)
            await refresh()
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    // MARK: - Connected apps (NIP-46 bunker) — the invite start

    /// Mint a connect invite: the reply is the ONE-TIME reveal of the
    /// `bunker://…` connect string — the nest never shows it again. It creates a
    /// pending connection, which is a row of the Connected apps page.
    public func connectApp() async {
        guard let api else { return }
        errorMessage = nil
        do {
            bunkerInvite = try await api.nostrBunkerCreateInvite()
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    // MARK: - Zap signers (NIP-57 trust root)

    // Excised with the money plane — same condition and same reason as the
    // state block above.
    #if !FAUNA_EXCISE_PAYMENTS

    /// Re-fetch this actor's designated zap signers and rebuild the list —
    /// called from `refresh()` and after add/remove (mirrors
    /// `refreshBunkerApps`/linux's `refresh_zap_signers`).
    public func refreshZapSigners() async {
        guard let api else { return }
        do {
            zapSigners = try await api.nostrZapSignersList()
        } catch {
            zapSigners = []
        }
    }

    /// Designate a signer. The server always returns the STORED row (64-hex,
    /// lowercased), so this re-lists rather than pushing the typed input
    /// locally — the same reason linux/tui re-list after `add`.
    public func addZapSigner() async {
        guard let api else { return }
        let pubkey = zapSignerPubkeyInput.trimmingCharacters(in: .whitespaces)
        guard !pubkey.isEmpty else { return }
        errorMessage = nil
        do {
            _ = try await api.nostrZapSignersAdd(
                signerPubkey: pubkey,
                label: zapSignerLabelInput.trimmingCharacters(in: .whitespaces)
            )
            zapSignerPubkeyInput = ""
            zapSignerLabelInput = ""
            await refreshZapSigners()
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    /// Stop trusting a signer, keyed by its STORED pubkey (the row's own
    /// `signerPubkey`, not the typed input). Never gated — removal is
    /// de-escalation.
    public func removeZapSigner(pubkey: String) async {
        guard let api else { return }
        do {
            try await api.nostrZapSignersRemove(signerPubkey: pubkey)
            await refreshZapSigners()
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    /// Why `nostr-zap-signer-add-btn` is dead, or `nil` when it works.
    /// Mirrors linux's `zap_signer_add_gate_reason` / tui's `designate_gate`
    /// exactly: the decision is READ off the shared `FeaturesClient` rows
    /// (already accounts for the `payments` subset edge, never re-composed),
    /// and a `hidden` row still disables with its one-word status rather than
    /// vanishing (the *excision* story is the orthogonal compile-time `zaps`
    /// feature, not this render-time courtesy).
    private static func zapSignerAddGateReason(_ rows: [FfiFeatureRow]) -> String? {
        guard let row = rows.first(where: { $0.feature == "zaps" }) else { return nil }
        if row.affordance == "available" { return nil }
        if let restriction = row.restriction {
            return renderLocalizedTextNested(restriction)
        }
        return renderLocalizedText(row.status)
    }

    /// Re-fetch the gated-feature plane and apply the `zaps` verdict to
    /// `zapSignerAddGateReason` — called once from `refresh()`. Never
    /// disables eagerly before the fetch resolves (an un-hydrated read
    /// leaves the button live — the nest, not the app, is the enforcement
    /// floor).
    public func refreshZapSignerGate() async {
        guard let api else { return }
        do {
            let rows = try await api.featuresClient().rows()
            zapSignerAddGateReason = NostrVM.zapSignerAddGateReason(rows)
        } catch {
            // Leave the prior verdict (or nil) rather than surfacing a
            // page-level error over a courtesy affordance the nest itself
            // still enforces.
        }
    }

    #endif
}
