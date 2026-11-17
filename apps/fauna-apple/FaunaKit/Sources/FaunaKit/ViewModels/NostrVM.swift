// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
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
    /// The `zaps` plane's answer over the Dim-3 features courtesy read (the
    /// shared `features_gate_possibility`, tui's `designate_gate`, android's
    /// `NostrVM.zapSignerGate`): blocked still loading until the read answers,
    /// blocked load-failed when it threw, blocked with a `disabled` row's
    /// restriction, absent — the designate row left out — when the nest has no
    /// `zaps` row or hides it (`docs/goal/ui/README.md` § Offer only what is
    /// possible, rules 2 and 5). `remove` is never gated — de-escalation.
    public private(set) var zapSignerGate: Possibility = featureGateAnswer(rows: nil, feature: "zaps")

    /// `nostr-zap-signer-add-btn`'s draft half — the shared
    /// `zap_signer_add_possibility` (blank, or not 64 hex).
    public var zapSignerAddPossibility: Possibility {
        nostrZapSignerAddPossibility(pubkey: zapSignerPubkeyInput)
    }

    /// `nostr-zap-signer-add-btn`'s whole answer (tui's
    /// `zap_signer_add_answer`): the plane's gate — the more specific reason —
    /// ANDed with the draft's. ``addZapSigner()`` asks it again.
    public var zapSignerAddAnswer: Possibility {
        zapSignerGate.and(zapSignerAddPossibility)
    }
    #endif

    /// The *Connected apps* section renders only for a linked account in a
    /// custodial signing mode — a `remote`/`nip07` account has no key on the
    /// box to sign with, so it can never itself be a bunker. Mirrors web's
    /// `showConnectedApps` / android's `CUSTODIAL_MODES` gate.
    public var showConnectedApps: Bool { connectedAppsPossibility.isShown }

    /// The *Connected apps* section's answer — the shared
    /// `connected_apps_possibility` over the stored signing mode (`nil` with no
    /// linked account): absent for a `remote`/`nip07` account, never an
    /// app-local mode check (`nostr.md` § Errors & edge cases).
    public var connectedAppsPossibility: Possibility {
        nostrConnectedAppsPossibility(signingMode: status?.linked == true ? status?.mode : nil)
    }

    // MARK: - Form validity (`docs/goal/ui/README.md` § Offer only what is
    // possible, rule 4's form case; `nostr.md` § Errors & edge cases)
    //
    // Each submit paints from its shared draft-validity predicate through
    // `faunaPossible`, and its action asks the same predicate first and refuses
    // with the same reason on `error-message` (``refuse(_:)``).

    /// `nostr-link-button`: the import mode needs its nsec, the remote mode its
    /// bunker URL.
    public var linkPossibility: Possibility {
        nostrLinkPossibility(mode: linkMode.rawValue, nsec: importNsec, bunkerUrl: bunkerUrl)
    }

    /// `nostr-add-relay`: a URL, well formed, not already listed.
    public var addRelayPossibility: Possibility {
        nostrAddRelayPossibility(existing: relays, input: relayInput)
    }

    /// `nostr-add-follow`: the pubkey (the petname is optional).
    public var addFollowPossibility: Possibility {
        nostrAddFollowPossibility(pubkey: followPubkey)
    }

    /// Refuse a submit whose answer is not possible, with its reason on
    /// `error-message`. `true` when refused.
    func refuse(_ answer: Possibility) -> Bool {
        guard let refusal = answer.refusal else { return false }
        errorMessage = refusal
        return true
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
                zapSignerGate = featureGateAnswer(rows: nil, feature: "zaps")
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
        if refuse(linkPossibility) { return }
        guard let api else { return }
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }
        do {
            switch linkMode {
            case .generate:
                _ = try await api.linkNostrGenerate()
            case .import:
                _ = try await api.linkNostrImport(nsec: importNsec)
                importNsec = ""
            case .remote:
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
        // The shared predicate names a blank, malformed or duplicate draft;
        // the helpers below only shape the list it already vouched for.
        if refuse(addRelayPossibility) { return }
        guard let url = trimmedRelayInput(input: relayInput),
              let next = relayListAppending(existing: relays, url: url) else { return }
        errorMessage = nil
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
        if refuse(addFollowPossibility) { return }
        guard let api else { return }
        let pubkey = followPubkey.trimmingCharacters(in: .whitespaces)
        errorMessage = nil
        do {
            let petname = followPetname.trimmingCharacters(in: .whitespaces)
            let queued = try await api.addNostrFollowOrQueue(
                pubkey: pubkey,
                petname: petname.isEmpty ? nil : petname
            )
            followPubkey = ""
            followPetname = ""
            if settleFollowChange(queued: queued) { await refresh() }
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    public func removeFollow(npub: String) async {
        guard let api else { return }
        do {
            let queued = try await api.removeNostrFollowOrQueue(npub: npub)
            if settleFollowChange(queued: queued) { await refresh() }
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    /// Settle one follow/unfollow's outcome; returns whether the page re-lists.
    /// Queued with no nest (the bridges follow composer, account-offline-mutation.md
    /// § The offline-mutation contract) → the page error clears and no list
    /// changes; sent → re-list. The same rule as ``BridgeManagerVM``'s.
    func settleFollowChange(queued: Bool) -> Bool {
        if queued { errorMessage = nil }
        return !queued
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
        if refuse(zapSignerAddAnswer) { return }
        guard let api else { return }
        let pubkey = zapSignerPubkeyInput.trimmingCharacters(in: .whitespaces)
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

    /// Re-fetch the gated-feature plane and answer ``zapSignerGate`` from the
    /// `zaps` row — called once from `refresh()`. Unknown is not possible: a
    /// fetch that threw blocks with load-failed (tui's `designate_gate`); the
    /// nest stays the enforcement floor either way.
    public func refreshZapSignerGate() async {
        guard let api else { return }
        do {
            let rows = try await api.featureRows()
            applyZapSignerFeaturesRead(rows)
        } catch {
            applyZapSignerFeaturesRead(nil)
        }
    }

    /// The features read landed (`rows`) or threw (`nil`). Internal so the
    /// unit tests drive it without a nest.
    func applyZapSignerFeaturesRead(_ rows: [FfiFeatureRow]?) {
        zapSignerGate = featureGateAnswer(rows: rows, failed: rows == nil, feature: "zaps")
    }

    #endif
}
#endif // !FAUNA_KIDS
