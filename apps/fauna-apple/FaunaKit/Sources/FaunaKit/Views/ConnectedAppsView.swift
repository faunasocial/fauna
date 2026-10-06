import SwiftUI

/// The shared **Connected apps** Settings sub-page (macOS + iOS, one FaunaKit
/// view) — `docs/goal/ui/connected-apps.md`; `ui.yaml` page `connected-apps`;
/// rail slot directly after *Task delegation* (`settings.md` § Navigation model).
/// Four regions, top to bottom:
///
/// 1. **Requests** — the quiet-push tray: one built consent card per live
///    request, with Approve / Decline / *Never show requests from this app*.
///    Painted only while a request is live — never a "no requests" row, which
///    would train the user to ignore the one place the anti-phishing check
///    happens.
/// 2. **Connect an app** — the typed-code start: a code field and a submit.
/// 3. **The roster** — one row per connected app, Revoke with an inline confirm;
///    a mail app-password row also carries its login, kind and secret controls.
/// 4. **Blocked apps** — one row per blocked client with Unblock; painted only
///    while something is blocked.
///
/// A paint shell over the shared `ConnectedAppsMachine` (``ConnectedAppsVM``):
/// the roster's composition, the scope words, the class badge key, *lasts-until*
/// and which verb revokes a row are all the machine's, so this file never picks a
/// revoke verb (a row's `key` is opaque) and never words a scope. The consent
/// card is the built card the AT Protocol page painted before the lift
/// (`atproto_settings.consent_*` wording unchanged) — one card, a new start,
/// never a second one.
///
/// **The lift.** The AT Protocol page's consent card and connected-app rows, the
/// Nostr page's bunker rows and the Mail & Calendar page's app-password rows
/// render HERE and no longer on their old pages — a row moves, it is never shown
/// twice (`connected-apps.md` § Architectural rules).
///
/// **Container = eager `ScrollView { VStack }`, NOT a lazy `Form`**, so every
/// element registers with the in-process automation driver regardless of scroll
/// position (`apple-e2e-automation.md` § Registration rules, rule 6 — a lazy
/// container pools rows and leaves a removed row readable). Reference painter:
/// tui `apps/fauna-tui/src/settings/connected_apps.rs`.
public struct ConnectedAppsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = ConnectedAppsVM()

    /// Bumped by the shell on every navigation to this page; a changed token
    /// restarts the visit (a rail re-selection of the page already on screen
    /// does not remount it, and rows are nest state read on every open).
    private let reloadToken: Int

    public init(reloadToken: Int = 0) {
        self.reloadToken = reloadToken
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.pageHeading, L.connectedApps.title)
                    .font(.title2)
                Text(L.connectedApps.description)
                    .font(.callout)
                    .foregroundStyle(.secondary)

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                // 1. Requests — only while a request is live.
                let requests = vm.snapshot?.requests ?? []
                if !requests.isEmpty {
                    sectionHeading(L.connectedApps.requestsHeading)
                    ForEach(Array(requests.enumerated()), id: \.element.consentIdHex) { index, request in
                        requestCard(request, index: index)
                    }
                }

                // 2. Connect an app.
                connectSection

                // 3. The roster. The three-state list: nothing until THIS visit's
                // roster read has returned, then either rows or the empty state
                // (`ui/README.md` § List pages).
                sectionHeading(L.connectedApps.rosterHeading)
                if let snapshot = vm.snapshot, snapshot.loaded {
                    if snapshot.principals.isEmpty {
                        automationText(Ids.connectedAppsEmpty, L.connectedApps.empty)
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                    ForEach(Array(snapshot.principals.enumerated()), id: \.element.key) { index, row in
                        rosterRow(row, index: index)
                    }
                }

                // 4. Blocked apps — only while something is blocked.
                let blocked = vm.snapshot?.blocked ?? []
                if !blocked.isEmpty {
                    sectionHeading(L.connectedApps.blockedHeading)
                    Text(L.connectedApps.blockedHint)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    ForEach(Array(blocked.enumerated()), id: \.element.clientId) { index, row in
                        blockedRow(row, index: index)
                    }
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityIdentifier(Ids.connectedApps)
        .automationValue(Ids.connectedApps, text: { "" })
        .pageTitle(L.connectedApps.title)
        .task(id: reloadToken) {
            guard let client else { return }
            await vm.visit(api: client.api, handle: client.sessionMaterial?.handle ?? "")
        }
    }

    private func sectionHeading(_ title: String) -> some View {
        Text(title).font(.headline)
    }

    // MARK: - Connect an app

    private var connectSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            sectionHeading(L.connectedApps.connectHeading)
            Text(L.connectedApps.connectHint)
                .font(.caption)
                .foregroundStyle(.secondary)
            TextField(L.connectedApps.connectPlaceholder, text: $vm.code)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.connectedAppsConnectCode)
                .automationField(Ids.connectedAppsConnectCode, text: $vm.code)
            // Typing a code claims a live, minutes-long request on the nest, so the
            // submit is an online-only commit; the field itself is a buffer and
            // stays live.
            Button(L.connectedApps.connectSubmit) {
                Task { await vm.submitCode() }
            }
            .disabled(vm.code.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            .accessibilityIdentifier(Ids.connectedAppsConnectSubmit)
            .automationActivate(
                Ids.connectedAppsConnectSubmit,
                isEnabled: { !vm.code.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
            ) {
                Task { await vm.submitCode() }
            }
            .faunaGate("fauna.oauth.consent.lookup_code")
            FaunaOfflineReason("fauna.oauth.consent.lookup_code")
        }
    }

    // MARK: - Requests tray

    /// One `connected-apps-request-card`: a third-party app is asking to act for
    /// this account and is waiting for the answer — the built consent card, moved
    /// here from the AT Protocol page with its wording unchanged. Every request
    /// the nest lists is painted, an unhinted browser request included (listed to
    /// every account by design — the binding code is the user's check).
    ///
    /// Three things render, each load-bearing:
    /// - **who is asking** — the resolved `clientName` plus the `clientId`
    ///   VERBATIM. There is deliberately no logo: `logo_uri` never crosses to the
    ///   app at all, because loading an attacker-named URL here would disclose the
    ///   user's address to whoever published the client's metadata document.
    /// - **what it wants** — one line per `scopeDescriptions` entry, worded by the
    ///   shared `authz::describe_scope` the browser's own consent page renders
    ///   from (never re-worded here), then the permission sets' provenance after
    ///   the effective list rather than instead of it.
    /// - **the binding code** — minted by the nest, so this value and the
    ///   browser's have one origin; carried in a `code` attr (not just prose), the
    ///   value the user actually compares.
    ///
    /// ⚠ Both answer controls render unconditionally — **never gated on how close
    /// the request is to expiring**: a resolution is reported even past
    /// `expires_at`, and the row carries no timestamp to check against, which is
    /// what keeps that ruling structural rather than remembered.
    private func requestCard(_ request: ConsentCardRow, index: Int) -> some View {
        let who = request.clientName.map {
            L.atprotoSettings.consentClient(name: $0, clientId: request.clientId)
        } ?? L.atprotoSettings.consentClientUnnamed(clientId: request.clientId)
        let asks = request.scopeDescriptions.map { "  • \($0)" }.joined(separator: "\n")
        let setLines = consentSetLines(request.sets)
        let cardText = "\(L.atprotoSettings.consentHeading)\n\(who)\n"
            + "\(L.atprotoSettings.consentScopesHeading)\n\(asks)"
            + setLines.map { "\n\($0)" }.joined()
        let codeText = L.atprotoSettings.consentCode(code: request.code)
        return VStack(alignment: .leading, spacing: 6) {
            Text(L.atprotoSettings.consentHeading).font(.headline)
            Text(who)
            Text(L.atprotoSettings.consentScopesHeading)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(asks).font(.caption)

            // Permission-set provenance, AFTER the effective list rather than
            // instead of it: every member is already in `asks` above. Empty
            // (nothing paints) for the common set-less request.
            ForEach(Array(setLines.enumerated()), id: \.offset) { _, line in
                Text(line).font(.caption2).foregroundStyle(.secondary)
            }

            Text(codeText)
                .accessibilityIdentifier(Ids.connectedAppsRequestCode)
                .automationValue(
                    Ids.connectedAppsRequestCode,
                    text: { codeText },
                    value: { request.code },
                    attributes: { ["code": request.code] }
                )
            Text(L.atprotoSettings.consentCodeHint)
                .font(.caption2)
                .foregroundStyle(.secondary)

            // ONE verdict for the pair: both outcomes are the SAME wire call with a
            // different boolean, so they issue one kind. Deny is not a local
            // dismiss — the waiting browser only gets a clean refusal once the
            // answer reaches the nest, which is why the "no" gates alongside the
            // "yes".
            HStack {
                Button(L.atprotoSettings.consentApproveButton) {
                    Task { await vm.resolveRequest(consentIdHex: request.consentIdHex, approved: true) }
                }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier(Ids.connectedAppsRequestApprove)
                .automationActivate(Ids.connectedAppsRequestApprove) {
                    Task { await vm.resolveRequest(consentIdHex: request.consentIdHex, approved: true) }
                }
                .faunaGate("fauna.bridges.atproto.resolve_consent")
                Button(L.atprotoSettings.consentDenyButton, role: .destructive) {
                    Task { await vm.resolveRequest(consentIdHex: request.consentIdHex, approved: false) }
                }
                .accessibilityIdentifier(Ids.connectedAppsRequestDecline)
                .automationActivate(Ids.connectedAppsRequestDecline) {
                    Task { await vm.resolveRequest(consentIdHex: request.consentIdHex, approved: false) }
                }
                .faunaGate("fauna.bridges.atproto.resolve_consent")
            }
            FaunaOfflineReason("fauna.bridges.atproto.resolve_consent")
            Button(L.connectedApps.block) {
                Task { await vm.blockRequest(consentIdHex: request.consentIdHex) }
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier(Ids.connectedAppsRequestBlock)
            .automationActivate(Ids.connectedAppsRequestBlock) {
                Task { await vm.blockRequest(consentIdHex: request.consentIdHex) }
            }
            .faunaGate("fauna.oauth.consent.block_client")
            FaunaOfflineReason("fauna.oauth.consent.block_client")
        }
        .padding()
        .background(Color.secondary.opacity(0.08))
        .clipShape(RoundedRectangle(cornerRadius: 8))
        // `.contain` keeps the card's own id AND every child id queryable — a bare
        // container id would clobber them.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.connectedAppsRequestCard)
        .automationValue(Ids.connectedAppsRequestCard, text: { cardText })
        .automationScope(Ids.connectedAppsRequestCard, index: index)
    }

    // MARK: - Roster

    /// The class badge's words — the one per-app half of the grouping key, which
    /// the machine derives. An unknown class paints no badge rather than a guess.
    private func classLabel(_ cls: String) -> String? {
        switch cls {
        case "remote": L.connectedApps.classRemote
        case "device": L.connectedApps.classDevice
        case "wasm": L.connectedApps.classWasm
        case "container": L.connectedApps.classContainer
        case "app_password": L.connectedApps.classAppPassword
        case "signer": L.connectedApps.classSigner
        case "oauth": L.connectedApps.classOauth
        default: nil
        }
    }

    /// One `connected-apps-item` roster row. The joined description is the item's
    /// own text (ui.yaml mints no per-field leaves for the columns every row has);
    /// a mail app password's own leaves and the Revoke controls are its children.
    ///
    /// A burned row (the identity-succession burn) has no leaf of its own: its
    /// `Access revoked` words sit directly under the name and the item carries a
    /// `revoked="true"` attr the tests read.
    private func rosterRow(_ row: ConnectedAppRow, index: Int) -> some View {
        let name = renderLocalizedText(row.name)
        let badge = classLabel(row.class)
        let head = badge.map { "\(name) · \($0)" } ?? name
        let burned = row.mail?.revoked == true
        let burnedLine = L.settings.mail.credentialRevoked
        let publisherLine: String? = {
            guard let clientId = row.clientId, let publisher = row.publisher else { return nil }
            return L.connectedApps.publisher(domain: publisher) + " — " + clientId
        }()
        let scopes = row.scopeDescriptions.map { "  • \(renderLocalizedText($0))" }
        var facts: [String] = []
        if !row.connected { facts.append(L.connectedApps.notConnected) }
        facts.append(L.connectedApps.created(time: formatUnixLocalMs(ms: row.createdAtMillis)))
        facts.append(
            row.lastUsedAtMillis.map { L.connectedApps.lastUsed(time: formatUnixLocalMs(ms: $0)) }
                ?? L.connectedApps.neverUsed)
        facts.append(
            row.lastsUntilMillis.map { L.connectedApps.lastsUntil(time: formatUnixLocalMs(ms: $0)) }
                ?? L.connectedApps.openEnded)
        let factsLine = facts.joined(separator: " · ")
        var itemLines = [head]
        if burned { itemLines.append(burnedLine) }
        if let publisherLine { itemLines.append(publisherLine) }
        itemLines.append(contentsOf: scopes)
        itemLines.append(factsLine)
        let itemText = itemLines.joined(separator: "\n")
        let armed = vm.revokeArmed == row.key

        return VStack(alignment: .leading, spacing: 6) {
            Text(head).font(.headline)
            if burned {
                Text(burnedLine).font(.caption).foregroundStyle(.red)
            }
            if let publisherLine {
                Text(publisherLine).font(.caption)
            }
            ForEach(Array(scopes.enumerated()), id: \.offset) { _, line in
                Text(line).font(.caption)
            }
            Text(factsLine).font(.caption).foregroundStyle(.secondary)

            if let mail = row.mail {
                mailLeaves(key: row.key, mail: mail)
            }

            if armed {
                Text(L.connectedApps.revokePrompt(name: name)).font(.caption)
                // The confirm is the commit. A mail app password's revoke is a
                // write to this client's own config (offline-safe — the same
                // ruling the Mail & Calendar page's revoke carried), so only the
                // nest-side verbs the machine picks for every other row gate.
                HStack {
                    confirmRevokeButton(row)
                    Button(L.connectedApps.revokeCancel) { vm.cancelRevoke() }
                        .accessibilityIdentifier(Ids.connectedAppsItemRevokeCancel)
                        .automationActivate(Ids.connectedAppsItemRevokeCancel) { vm.cancelRevoke() }
                }
                if row.mail == nil { FaunaOfflineReason("fauna.principals.revoke") }
            } else {
                Button(L.connectedApps.revoke, role: .destructive) { vm.armRevoke(row.key) }
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.connectedAppsItemRevoke)
                    .automationActivate(Ids.connectedAppsItemRevoke) { vm.armRevoke(row.key) }
            }
        }
        .padding()
        .background(Color.secondary.opacity(0.08))
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.connectedAppsItem)
        .automationValue(
            Ids.connectedAppsItem,
            text: { itemText },
            attributes: { burned ? ["revoked": "true"] : [:] }
        )
        .automationScope(Ids.connectedAppsItem, index: index)
    }

    /// The confirm is the commit. A mail app password's revoke never gates (it is
    /// a write to this client's own config, OfflineSafe); every other row's is the
    /// nest-side principal revoke the machine picks, which does.
    @ViewBuilder
    private func confirmRevokeButton(_ row: ConnectedAppRow) -> some View {
        let button = Button(L.connectedApps.revokeConfirm, role: .destructive) {
            Task { await vm.confirmRevoke(row.key) }
        }
        .accessibilityIdentifier(Ids.connectedAppsItemRevokeConfirm)
        .automationActivate(Ids.connectedAppsItemRevokeConfirm) {
            Task { await vm.confirmRevoke(row.key) }
        }
        if row.mail == nil {
            button.faunaGate("fauna.principals.revoke")
        } else {
            button
        }
    }

    /// The leaves only a mail app-password row carries: kind, login (+ copy) and
    /// the secret (+ reveal, copy). ⚠ The hidden secret's text MUST stay EMPTY:
    /// the cross-app driver polls it until it turns non-empty and returns that AS
    /// the secret, so a mask would pass the reveal test without the on-demand read
    /// ever running. The leaf is therefore always present, and only a revealed one
    /// holds words.
    private func mailLeaves(key: String, mail: MailAppPassword) -> some View {
        let username = vm.muaUsername(mail)
        let secret = vm.revealed[key]
        return VStack(alignment: .leading, spacing: 4) {
            automationText(Ids.connectedAppsItemType, renderLocalizedText(mail.kind))
                .font(.caption)
                .foregroundStyle(.secondary)
            HStack {
                automationText(Ids.connectedAppsItemUsername, username)
                    .font(.caption.monospaced())
                    .textSelection(.enabled)
                CopyButton(Ids.connectedAppsItemCopyUsername, text: username)
            }
            HStack {
                Text(secret ?? "")
                    .font(.caption.monospaced())
                    .textSelection(.enabled)
                    .accessibilityIdentifier(Ids.connectedAppsItemSecret)
                    .automationValue(Ids.connectedAppsItemSecret, text: { vm.revealed[key] ?? "" })
                    // Rule 2 of `security.md` § On-screen secret exposure: a
                    // minted, revocable credential suppresses capture while — and
                    // only while — it is actually revealed.
                    .suppressScreenCapture(isActive: secret != nil)
                Button(secret == nil ? L.settings.mail.revealSecret : L.settings.mail.hideSecret) {
                    Task { await vm.toggleReveal(key) }
                }
                .controlSize(.small)
                .accessibilityIdentifier(Ids.connectedAppsItemRevealSecret)
                .automationActivate(Ids.connectedAppsItemRevealSecret) {
                    Task { await vm.toggleReveal(key) }
                }
                // Copy is independent of the reveal toggle: the secret is read on
                // demand at click time, so it reaches the clipboard without being
                // painted on a screen someone else can read.
                Button {
                    copySecret(key)
                } label: {
                    Image(systemName: "doc.on.doc").font(.caption)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(L.settings.mail.copySecret)
                .accessibilityIdentifier(Ids.connectedAppsItemCopySecret)
                .automationActivate(Ids.connectedAppsItemCopySecret) { copySecret(key) }
            }
        }
    }

    private func copySecret(_ key: String) {
        Task {
            if let secret = await vm.readSecret(key) { Pasteboard.copy(secret) }
        }
    }

    // MARK: - Blocked apps

    /// One `connected-apps-blocked-item`: the client id **verbatim**, as the
    /// request card showed it — nothing here parses it into a host or a name —
    /// and when it was blocked, with Unblock.
    private func blockedRow(_ blocked: BlockedAppRow, index: Int) -> some View {
        let since = L.connectedApps.blockedSince(time: formatUnixLocalMs(ms: blocked.blockedAtMillis))
        return VStack(alignment: .leading, spacing: 6) {
            Text(blocked.clientId)
            Text(since).font(.caption).foregroundStyle(.secondary)
            Button(L.connectedApps.unblock) {
                Task { await vm.unblock(clientId: blocked.clientId) }
            }
            .controlSize(.small)
            .accessibilityIdentifier(Ids.connectedAppsBlockedItemUnblock)
            .automationActivate(Ids.connectedAppsBlockedItemUnblock) {
                Task { await vm.unblock(clientId: blocked.clientId) }
            }
        }
        .padding()
        .background(Color.secondary.opacity(0.08))
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.connectedAppsBlockedItem)
        .automationValue(Ids.connectedAppsBlockedItem, text: { "\(blocked.clientId)\n\(since)" })
        .automationScope(Ids.connectedAppsBlockedItem, index: index)
    }
}

/// The consent card's permission-set section as flat lines
/// (`atproto-pds-full.md` § F4 detail → *Permission sets*, the card bullet). One
/// heading per set — the publisher's title beside the NSID, or the NSID alone
/// when no title was declared (never an invented label) — then the publisher's
/// `details` when present, then one bulleted line per member. Empty for the
/// overwhelmingly common set-less request, so the card paints no heading and no
/// stray blank line. `title`/`details`/`memberDescriptions` arrive already
/// control-stripped and worded by the shared machine's one composition pass —
/// never re-word or re-fence here.
private func consentSetLines(_ sets: [ConsentSetRow]) -> [String] {
    var lines: [String] = []
    for set in sets {
        lines.append(set.title.map {
            L.atprotoSettings.consentSetHeading(title: $0, nsid: set.nsid)
        } ?? L.atprotoSettings.consentSetHeadingUnnamed(nsid: set.nsid))
        if let details = set.details { lines.append(details) }
        for member in set.memberDescriptions { lines.append("• \(member)") }
    }
    return lines
}
