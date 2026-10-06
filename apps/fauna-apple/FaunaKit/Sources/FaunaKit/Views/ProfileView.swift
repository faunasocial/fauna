import SwiftUI
import UniformTypeIdentifiers
import FaunaFFISwift

/// The Profile page — the canonical per-user *detail* surface (`profile.md`),
/// shared by macOS + iOS (one FaunaKit view, thin per-target mount points —
/// One view branches on `is_self` (mirrors linux
/// `build_profile_view(target)` + windows `ProfileViewModel(isSelf)`): the SELF
/// view (`actorId == nil`, reached via the top-level `profile-tab`) shows the
/// `profile-edit-button` + the Tiers-tab author management; ANOTHER actor's view
/// (`actorId` set, reached by a contact-row tap-through) shows the
/// `profile-follow-button` + the Tiers-tab subscriber-browse offers section.
/// Element IDs match `tests/e2e-unified/ui.yaml` `profile` exactly. Reference
/// renderer: linux (`apps/fauna-linux/src/views/profile/{mod,offers,edit}.rs`).
///
/// The rich-identity header `display_name` is read from the published `Profile`
/// (`fauna.profile.get`) when present, else it falls back to the handle (SELF) or
/// the hex actor_id (`profile.md` § State & data shape — publish-on-first-edit, so
/// a never-published actor renders its actor_id until it saves the edit form).
/// `ProfileView`'s `.task(id:)` key: the session (client instance + the tiers
/// reload token) paired with the actor being viewed. A plain array of `String`s
/// cannot carry the session, and `SessionKey` is `Equatable` but not `Hashable`, so
/// the two travel as this pair — `.task(id:)` needs only `Equatable`.
private struct ProfilePageKey: Equatable {
    let session: SessionKey
    let viewedActorId: String
}

public struct ProfileView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = SubscriptionsVM()
    @State private var offersVM = ProfileOffersVM()
    /// OTHER profile only — the request-contact knock and its guardian ask
    /// (`profile-request-contact-button`; `family-safety.md` § Child-initiated
    /// contact requests → *App affordance*).
    @State private var knockVM = ProfileKnockVM()
    /// OTHER profile only — the private section's staged nickname, notes and
    /// labels (`profile.md` § The private section).
    @State private var privateVM = ProfilePrivateVM()
    /// The typed-but-not-yet-added label (`profile-label-field`). App glue: the
    /// label itself is staged in shared Rust only once Add accepts it.
    @State private var labelInput = ""
    /// The owner of the conversations manager, and so of the private-overlay
    /// projection the header's names and the private section read.
    @Environment(ConversationsVM.self) private var conversationsVM: ConversationsVM?
    /// The app-root `fauna.family.status` projection the ward's durable "asked —
    /// waiting for your guardian" state reads from; `nil` where none is injected.
    @Environment(FamilyStatusStore.self) private var familyStatus: FamilyStatusStore?
    /// Shell-level singleton (like `FeedVM`/`ConversationsVM` on the App struct),
    /// not a page-local `@State`: the TestAgent's `compose.file` avatar/banner
    /// staging (no OS file-chooser can be driven headlessly) must reach the SAME
    /// instance this page renders `ProfileEditFormView` off — an
    /// `applyComposePatch` handler at the shell can't reach a page-local VM.
    @Environment(ProfileEditVM.self) private var editVM: ProfileEditVM?
    /// The published `display_name` (`fauna.profile.get`), `nil` until fetched or
    /// when the actor has not published.
    @State private var headerName: String?
    /// Active tab — "posts" (default landmark) or "tiers".
    @State private var activeTab = "posts"
    /// Bumped on every Tiers-tab activation (switch-to AND re-tap-while-active)
    /// so `.task(id:)` re-fires even when `activeTab` itself doesn't change —
    /// mirrors android's `tiersReloadToken` (`monetization.md` § Pillar 1 → *The
    /// Tiers-tab re-read door*, ruled 2026-08-12: every activation re-reads).
    @State private var tiersReloadToken = 0

    private let session: SessionState
    /// The viewed actor's hex id; `nil` ⇒ the SELF profile.
    private let actorId: String?
    /// Per-target nav glue for the OTHER-profile `profile-start-dm-button`: seed
    /// the Conversations new-thread composer with this actor and switch to the
    /// Conversations page. `nil` (the default) hides nothing — the button still
    /// renders, it just no-ops — but every real mount wires it (mirrors linux
    /// `on_open_conversations`). The actor is passed as a hex id; the call site
    /// owns the app routing (macOS `selectedSidebar` / iOS `selectedTab`).
    private let onStartDm: ((String) -> Void)?

    /// Block ⇄ unblock toggle state (OTHER profile). `isBlocked` is derived from
    /// the viewed actor's contact edge (`fauna.contacts.list`, read on open via
    /// `refreshBlockState`) — a `blocked` edge ⇒ the button reads "Unblock", else
    /// "Block"; the tap flips it over `fauna.knocks.{block,unblock}` and re-renders.
    /// `blocking` is the in-flight guard (disabled during the round-trip). A failed
    /// call leaves the state unchanged and re-enables for a retry. Mirrors linux
    /// `refresh_block_state` / `toggle_block` / `apply_block_state`.
    @State private var isBlocked = false
    @State private var blocking = false

    public init(
        session: SessionState,
        actorId: String? = nil,
        onStartDm: ((String) -> Void)? = nil
    ) {
        self.session = session
        self.actorId = actorId
        self.onStartDm = onStartDm
    }

    /// `true` for the viewer's own profile (edit + author management); `false`
    /// for another actor's (follow + subscriber-browse).
    private var isSelf: Bool { actorId == nil || actorId == session.actorId }

    /// The hex id of the actor whose profile is shown (the passed `actorId`, or
    /// the viewer's own when SELF).
    private var viewedActorId: String { actorId ?? session.actorId ?? "" }

    /// The SELF header identity label: the published `display_name` if known,
    /// else the handle, else the hex actor_id (mirrors linux `build_header` /
    /// `refresh_header_name`).
    private var selfHeaderDisplay: String {
        if let n = headerName, !n.isEmpty { return n }
        if let h = session.handle, !h.isEmpty { return h }
        return viewedActorId
    }

    /// The OTHER header's two names — the shared resolver's answer over the
    /// viewer's nickname, the published display name, then the canonical short
    /// id (`profile.md` § The private section → *The header shows both names*).
    /// The OTHER header holds no handle (mirrors linux `HeaderNames::label`).
    /// `nil` for SELF, which keeps its own one-line name.
    private var otherHeaderLabel: PeerLabel? {
        guard !isSelf, let conversationsVM else { return nil }
        return conversationsVM.contactOverlays.peerLabel(
            displayName: headerName, handle: nil, actorId: viewedActorId)
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header

            // Secondary relationship actions — OTHER profile only (profile.md
            // § Layout & flow): Start DM, Block, and the request-contact knock.
            if !isSelf {
                secondaryActionsRow
                privateSection
            }

            // SELF edit form — inline, revealed only after the base profile is
            // fetched (so the e2e's wait-for-form lands after the read).
            if isSelf, let editVM, editVM.isOpen {
                ProfileEditFormView(vm: editVM, onSaved: { await refreshHeader() })
            }

            automationText(Ids.pageHeading,
                           activeTab == "posts" ? L.profile.posts : L.profile.tiers)
                .font(.title2)
                .padding(.horizontal)

            tabStrip

            if let error = currentError {
                ErrorBanner(message: error)
                    .padding(.horizontal)
            }

            if activeTab == "posts" {
                Text(L.profile.noPosts)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal)
                Spacer()
            } else if isSelf {
                SubscriptionTiersTab(vm: vm)
            } else {
                SubscriptionOffersTab(vm: offersVM)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        // Page landmark — registered so the driver's is_visible/count see it.
        .automationValue(Ids.profileView)
        // Keyed on the SESSION as well as the viewed actor: iOS reaches this page as
        // a More destination, which the switch teardown does not unmount, and keying
        // on `viewedActorId` alone could not see the identity change at all for an
        // OTHER profile (the actor being viewed does not change when the viewer
        // does). `ProfileOffersVM`'s `statusTier`/`isFollowing` are the VIEWER's own
        // relationship to that creator, so a stale one tells the incoming account it
        // holds a paid tier on the strength of the outgoing account's subscription;
        // `SubscriptionsVM` carries the author's subscriber roster and the §4
        // provider secret. `account-scoping.md` § The scoping taxonomy, the "reused
        // shell" case; macOS unmounts the window shell wholesale, so there the drop
        // is redundant — carried for uniformity, as `SearchVM`'s is
        // .
        .task(id: ProfilePageKey(session: SessionKey(client, reloadToken: tiersReloadToken),
                                 viewedActorId: viewedActorId)) {
            guard let client else {
                // BOTH branches' view models drop: which branch is live depends on
                // `isSelf`, which is itself derived from the session that just went
                // away, so dropping only the current branch's would leave the other's
                // state for the incoming account to re-enter into.
                vm.reset()
                offersVM.reset()
                knockVM.reset()
                privateVM.reset()
                labelInput = ""
                return
            }
            // One private-section editor per open of one person's profile, over
            // the manager as it stands now: nothing staged for the last person
            // (or by the last account) carries over.
            labelInput = ""
            if !isSelf, let conversationsVM {
                privateVM.open(editor: conversationsVM.contactOverlays.editor(actorId: viewedActorId))
            } else {
                privateVM.reset()
            }
            if isSelf {
                await vm.configure(api: client.api, authorIdHex: viewedActorId)
            } else {
                await offersVM.configure(api: client.api, authorIdHex: viewedActorId)
                if let selfActorId = session.actorId {
                    knockVM.configure(api: client.api, actorId: selfActorId, familyStatus: familyStatus)
                }
                // A fresh open: the knock's per-open state (error, "sent", route)
                // starts clean, as a rebuilt page would.
                knockVM.beginOpen(peer: viewedActorId)
                await refreshBlockState()
            }
            await refreshHeader()
        }
        // An untouched private section follows a sibling device's edit: re-read
        // the form whenever the overlay projection moves.
        .onChange(of: conversationsVM?.overlayRevision) { privateVM.reload() }
    }

    /// The page-level error — the active branch's VM (`error-message`). The
    /// private section's refusals ride the same element.
    private var currentError: String? {
        isSelf ? vm.errorMessage
               : (privateVM.errorMessage ?? knockVM.errorMessage ?? offersVM.errorMessage)
    }

    /// Re-read the published `display_name` for the header (publish-on-first-edit;
    /// `not_found` / decode failure ⇒ fall back to handle/actor_id via
    /// `headerDisplay`). Called on appear + after a SELF edit-form save.
    private func refreshHeader() async {
        guard let client else { return }
        if let body = try? await client.api.profileGet(actorId: viewedActorId),
           let display = try? client.api.decodeProfile(body: body) {
            headerName = display.displayName
            // OTHER profile: the knock's route is the shared rule over the profile
            // this open just fetched — no second fetch (`profile.md` § Where logic
            // lives → *Request contact routing*). Keyed on the peer the body is
            // ABOUT, so a read that outlives its open cannot route the next actor.
            if !isSelf { knockVM.noteProfile(body: body, peer: viewedActorId) }
        } else {
            headerName = nil
        }
    }

    // MARK: - Header (shared `user-header`)

    private var header: some View {
        HStack(spacing: 12) {
            Image(systemName: "person.crop.circle")
                .resizable()
                .frame(width: 48, height: 48)
                .foregroundStyle(.secondary)

            // With a nickname set, the primary line is the nickname and the
            // public name it replaced stays visible beneath — the page never
            // hides who this actually is.
            VStack(alignment: .leading, spacing: 2) {
                automationText(Ids.profileHandle, otherHeaderLabel?.primary ?? selfHeaderDisplay)
                    .font(.title3)
                if let publicName = otherHeaderLabel?.public {
                    automationText(Ids.profilePublicName, publicName)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            CopyButton(Ids.profileActorIdCopyBtn, text: viewedActorId)

            // The primary relationship action branches on is_self (profile.md
            // § Layout & flow): SELF → edit form; another's → follow (= subscribe
            // to the free "followers" tier).
            if isSelf {
                Button(L.profile.edit) { openEditForm() }
                    .accessibilityIdentifier(Ids.profileEditButton)
                    .automationActivate(Ids.profileEditButton) { openEditForm() }
            } else {
                // Label flips "Follow" ⇄ "Following" off the shared
                // `followToggleLabel` (profile.md § Where logic lives → Follow /
                // unfollow), resolved the same way the block toggle resolves
                // `contactToggleBlockLabel`. `isFollowing` flips optimistically on a
                // successful follow.
                Button(renderLocalizedText(followToggleLabel(isFollowing: offersVM.isFollowing))) {
                    Task { await offersVM.follow() }
                }
                .accessibilityIdentifier(Ids.profileFollowButton)
                .automationActivate(
                    Ids.profileFollowButton,
                    value: { renderLocalizedText(followToggleLabel(isFollowing: offersVM.isFollowing)) }
                ) {
                    Task { await offersVM.follow() }
                }
            }
        }
        .padding([.top, .horizontal])
    }

    // MARK: - Secondary relationship actions (OTHER profile only)

    /// Start-DM, Block and Request contact — the ratified OTHER-profile secondary
    /// actions (`profile.md` § User actions) — and the ward's guardian ask beneath
    /// them. Lifted from linux (`apps/fauna-linux/src/views/profile/mod.rs`
    /// `!is_self` actions row) so both apple apps render the identical shape.
    @ViewBuilder
    private var secondaryActionsRow: some View {
        HStack(spacing: 8) {
            // Start DM — pure nav glue (no new kind, no persistence). The shared
            // seed lives in `ConversationsVM.startDirectMessage`; the per-target
            // `onStartDm` switches the app to the Conversations page.
            Button(L.profile.startDm) { onStartDm?(viewedActorId) }
                .accessibilityIdentifier(Ids.profileStartDmButton)
                .automationActivate(Ids.profileStartDmButton) { onStartDm?(viewedActorId) }

            // Block ⇄ Unblock toggle — the label flips on the viewed actor's
            // contact edge (`contact_status`, read on open via `refreshBlockState`).
            // A not-blocked edge → "Block" (destructive) over `fauna.knocks.block`
            // (upsert → blocked); a blocked edge → "Unblock" (neutral) over
            // `fauna.knocks.unblock` (the guarded clear-the-edge, `ContactStatus`
            // → `None`; `contacts.md` § Where logic lives → Unblock). Disabled only
            // during the round-trip; a failed call re-enables for a retry. Mirrors
            // linux `toggle_block` / `apply_block_state`.
            Button(renderLocalizedText(contactToggleBlockLabel(isBlocked: isBlocked)),
                   role: isBlocked ? nil : .destructive) {
                toggleBlock()
            }
            .disabled(blocking)
            .accessibilityIdentifier(Ids.profileBlockButton)
            .automationActivate(
                Ids.profileBlockButton,
                isEnabled: { !blocking },
                value: { renderLocalizedText(contactToggleBlockLabel(isBlocked: isBlocked)) }
            ) { toggleBlock() }

            // Request contact — the knock, sent from the profile the user is already
            // looking at (`profile.md` § Layout & flow). The label flips to "Request
            // sent" once the nest accepts it, and the button disables with it. The
            // profile is a CALLER of the contact lifecycle (`fauna.inbox.send`), not a
            // second implementation: ``ProfileKnockVM`` sends it through the same
            // shared writer the Contacts page uses.
            Button(requestContactLabel) { requestContact() }
                .disabled(knockVM.isSent(peer: viewedActorId))
                .accessibilityIdentifier(Ids.profileRequestContactButton)
                .automationActivate(
                    Ids.profileRequestContactButton,
                    isEnabled: { !knockVM.isSent(peer: viewedActorId) },
                    value: { requestContactLabel }
                ) { requestContact() }
        }
        .padding(.horizontal)

        // The ward's ask (`family-safety.md` § Child-initiated contact requests →
        // *App affordance*): the Contacts page's pair, on this page too. The button
        // only after the nest's TYPED guardian refusal; the pending label once one is
        // outstanding.
        ContactAskRow(state: knockVM.askState(peer: viewedActorId)) {
            let peer = viewedActorId
            Task { await knockVM.askGuardian(peer: peer) }
        }
        .padding(.horizontal)
    }

    // MARK: - The private section (OTHER profile only)

    /// The viewer's own nickname, notes and labels on this person (`profile.md`
    /// § The private section): below the relationship actions, above the tab
    /// strip. Every gesture only stages; one Save commits. The staging, the
    /// diff, the bounds and every refusal are shared Rust behind
    /// ``ProfilePrivateVM`` — this owns the widgets only.
    private var privateSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.profile.privateTitle)
                .font(.subheadline.weight(.semibold))

            let nickname = Binding(get: { privateVM.form.nickname },
                                   set: { privateVM.setNickname($0) })
            TextField(L.profile.privateNickname, text: nickname)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.profileNicknameField)
                .automationField(Ids.profileNicknameField, text: nickname)

            let notes = Binding(get: { privateVM.form.notes },
                                set: { privateVM.setNotes($0) })
            TextField(L.profile.privateNotes, text: notes, axis: .vertical)
                .lineLimit(3...8)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.profileNotesField)
                .automationField(Ids.profileNotesField, text: notes)

            Text(L.profile.privateLabels)
                .font(.caption)
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(privateVM.form.labels.enumerated()), id: \.offset) { index, label in
                    HStack {
                        automationText(Ids.profileLabelChip, label)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Button(L.profile.privateLabelRemove) { privateVM.removeLabel(at: index) }
                            .controlSize(.small)
                            .accessibilityIdentifier(Ids.profileLabelRemoveButton)
                            .automationActivate(Ids.profileLabelRemoveButton) {
                                privateVM.removeLabel(at: index)
                            }
                    }
                }
            }
            // List landmark; the chips and their remove buttons are flat-indexed.
            .automationValue(Ids.profileLabelList)

            HStack(spacing: 8) {
                TextField(L.profile.privateLabelAdd, text: $labelInput)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.profileLabelField)
                    .automationField(Ids.profileLabelField, text: $labelInput)
                Button(L.profile.privateLabelAdd) { addLabel() }
                    .accessibilityIdentifier(Ids.profileLabelAddButton)
                    .automationActivate(Ids.profileLabelAddButton) { addLabel() }
            }

            Button(L.profile.privateSave) { savePrivate() }
                .disabled(privateVM.isSaving)
                .accessibilityIdentifier(Ids.profilePrivateSaveButton)
                .automationActivate(Ids.profilePrivateSaveButton,
                                    isEnabled: { !privateVM.isSaving }) { savePrivate() }
        }
        .padding(.horizontal)
        // Section landmark — registered so the driver's is_visible sees it.
        .automationValue(Ids.profilePrivateSection)
    }

    /// Stage the typed label. The add field empties only when the label was
    /// staged, so a refused one stays typed beside its refusal.
    private func addLabel() {
        if privateVM.addLabel(labelInput) { labelInput = "" }
    }

    private func savePrivate() {
        Task { await privateVM.save() }
    }

    /// The request-contact button's label: "Request contact", then "Request sent".
    private var requestContactLabel: String {
        knockVM.isSent(peer: viewedActorId) ? L.profile.requestContactSent : L.profile.requestContact
    }

    /// Send the knock to the viewed actor. The peer is captured HERE, before the
    /// await, so a reply that outlives this open lands in that peer's own slot.
    private func requestContact() {
        let peer = viewedActorId
        Task { await knockVM.knock(peer: peer) }
    }

    /// Read the viewed actor's current contact edge (`fauna.contacts.list`) and set
    /// the toggle's initial state: a `blocked` edge ⇒ "Unblock", any other/absent
    /// status ⇒ "Block". Called on OTHER-profile appear (mirrors linux
    /// `refresh_block_state`; `contacts.md` § Persistence — match the hex `peer_id`).
    private func refreshBlockState() async {
        guard let client else { return }
        isBlocked = (try? await client.api.fetchContacts(actorId: viewedActorId))?
            .contains { contactRowBlocksActor(rowPeerId: $0.peerId, rowStatus: $0.status, targetActorId: viewedActorId) } ?? false
    }

    /// Block ⇄ unblock the viewed actor (`profile.md` § User actions; `contacts.md`
    /// § Where logic lives → Unblock). A not-blocked edge taps `fauna.knocks.block`
    /// (upsert → blocked); a blocked edge taps `fauna.knocks.unblock` (the guarded
    /// clear-the-edge). Disabled during the round-trip; on success the state flips
    /// and the label re-renders; on failure the state is unchanged so the user can
    /// retry. Shared by the `Button` + its `.automationActivate` (multi-statement
    /// `Task`, extracted per convention). The nest uses only `peerId` (the target).
    private func toggleBlock() {
        guard let client, !blocking else { return }
        let wasBlocked = isBlocked
        blocking = true
        Task {
            do {
                if wasBlocked {
                    try await client.api.unblockKnock(
                        actorId: session.actorId ?? "", peerId: viewedActorId)
                } else {
                    try await client.api.blockKnock(
                        actorId: session.actorId ?? "", peerId: viewedActorId)
                }
                isBlocked = !wasBlocked
            } catch {
                // Leave `isBlocked` unchanged so the button re-enables for a retry.
            }
            blocking = false
        }
    }

    /// Open the SELF edit form — fetches the current profile (the read-modify-write
    /// base) then reveals the form. Shared by the edit `Button` + its
    /// `.automationActivate` (multi-statement `Task`, extracted per convention).
    private func openEditForm() {
        guard let client, let editVM else { return }
        Task { await editVM.open(api: client.api, actorId: viewedActorId) }
    }

    // MARK: - Tab strip

    private var tabStrip: some View {
        HStack(spacing: 8) {
            Button(L.profile.posts) { activeTab = "posts" }
                .accessibilityIdentifier(Ids.profilePostsTab)
                .automationActivate(Ids.profilePostsTab) { activeTab = "posts" }
            Button(L.profile.tiers) { activeTab = "tiers"; tiersReloadToken += 1 }
                .accessibilityIdentifier(Ids.profileTiersTab)
                .automationActivate(Ids.profileTiersTab) { activeTab = "tiers"; tiersReloadToken += 1 }
        }
        .padding(.horizontal)
    }
}

// MARK: - Tiers tab (SELF author management — §1 / §2 / §3)

/// The three SELF author-management sections. A dumb renderer of
/// `SubscriptionsVM`; mutations re-read through the VM (observer-free).
private struct SubscriptionTiersTab: View {
    @Bindable var vm: SubscriptionsVM

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                myTiersSection
                pendingRequestsSection
                subscribersSection
                // §§4-5 are the money plane and excise with it
                // (`dynamic-features.md` § Platform-family surface excision).
                // ⚠ Gating only the API call would leave THIS render compiling
                // against empty state and still shipping every
                // `subscription-provider-*` / `subscription-claim-*` id —
                // criterion 1 is a `strings`-grep, and dead is not absent.
                #if !FAUNA_EXCISE_PAYMENTS
                paymentProvidersSection
                claimsSection
                #endif
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // ── §1 My tiers ─────────────────────────────────────────────────────

    private var myTiersSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.subscriptions.myTiers).font(.headline)

            Button(L.subscriptions.createTier) { vm.openCreateForm() }
                .accessibilityIdentifier(Ids.subscriptionTierCreateButton)
                .automationActivate(Ids.subscriptionTierCreateButton) { vm.openCreateForm() }

            if vm.showForm {
                tierForm
            }

            if vm.myTiers.isEmpty {
                Text(L.subscriptions.noTiers)
                    .font(.caption).foregroundStyle(.secondary)
            }
            ForEach(vm.myTiers, id: \.name) { tier in
                SubscriptionTierRow(tier: tier, vm: vm)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(Ids.subscriptionTierRow)
                    .automationValue(Ids.subscriptionTierRow, text: { tier.name })
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.subscriptionTiersSection)
        .automationValue(Ids.subscriptionTiersSection, text: { String(vm.myTiers.count) })
    }

    /// The inline create/edit form (`subscription-tier-form`). One form serves
    /// both; the name field is read-only while editing (server key).
    private var tierForm: some View {
        VStack(alignment: .leading, spacing: 6) {
            EditableFieldRow(L.subscriptions.tierName, "subscription-tier-form-name",
                             $vm.formName, labelColor: .secondary, maxWidth: 240,
                             trailingAlign: false, rowAlignment: .center,
                             disabled: vm.editingTier != nil)
            EditableFieldRow(L.subscriptions.rank, "subscription-tier-form-rank", $vm.formRank,
                             labelColor: .secondary, maxWidth: 240,
                             trailingAlign: false, rowAlignment: .center)
            EditableFieldRow(L.subscriptions.description, "subscription-tier-form-description",
                             $vm.formDescription, labelColor: .secondary, maxWidth: 240,
                             trailingAlign: false, rowAlignment: .center)
            EditableFieldRow(L.subscriptions.priceHint, "subscription-tier-form-price-hint",
                             $vm.formPriceHint, labelColor: .secondary, maxWidth: 240,
                             trailingAlign: false, rowAlignment: .center)
            #if !FAUNA_EXCISE_PAYMENTS
            // The machine-comparable threshold (monetization.md § The asking
            // price) — the money plane's author half, excised with §§4-5
            // below. Independent of price_hint above: never inferred from it.
            EditableFieldRow(L.subscriptions.askingPrice, "subscription-tier-form-asking-price",
                             $vm.formAskingPrice, labelColor: .secondary, maxWidth: 240,
                             trailingAlign: false, rowAlignment: .center)
            #endif
            EditableFieldRow(L.subscriptions.paymentUrl, "subscription-tier-form-payment-url",
                             $vm.formPaymentUrl, labelColor: .secondary, maxWidth: 240,
                             trailingAlign: false, rowAlignment: .center)

            HStack {
                Text(L.subscriptions.autoApprove).foregroundStyle(.secondary)
                Toggle("", isOn: $vm.formAutoApprove)
                    .labelsHidden()
                    .accessibilityIdentifier(Ids.subscriptionTierFormAutoApprove)
                    .automationActivate(Ids.subscriptionTierFormAutoApprove,
                                        value: { vm.formAutoApprove ? "on" : "off" }) {
                        vm.formAutoApprove.toggle()
                    }
            }

            HStack {
                Spacer()
                Button(L.subscriptions.cancel) { vm.cancelForm() }
                    .accessibilityIdentifier(Ids.subscriptionTierFormCancel)
                    .automationActivate(Ids.subscriptionTierFormCancel) { vm.cancelForm() }
                Button(L.subscriptions.save) { Task { await vm.saveForm() } }
                    .accessibilityIdentifier(Ids.subscriptionTierFormSave)
                    .automationActivate(Ids.subscriptionTierFormSave) {
                        Task { await vm.saveForm() }
                    }
            }
        }
        .padding(.vertical, 6)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.subscriptionTierForm)
        // Register the form container so the in-process driver's is_visible sees
        // it (registry-only); the form mounts/unmounts with vm.showForm.
        .automationValue(Ids.subscriptionTierForm)
    }

    // ── §2 Pending requests ─────────────────────────────────────────────

    private var pendingRequestsSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.subscriptions.pendingRequests).font(.headline)

            if vm.isApproving {
                automationText(Ids.subscriptionRequestBusy, L.subscriptions.approving)
                    .font(.caption).foregroundStyle(.secondary)
            }

            if vm.requests.isEmpty {
                Text(L.subscriptions.noRequests)
                    .font(.caption).foregroundStyle(.secondary)
            }
            ForEach(Array(vm.requests.enumerated()), id: \.element.requestId) { offset, request in
                SubscriptionRequestRow(request: request, vm: vm)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(Ids.subscriptionRequestRow)
                    .automationValue(Ids.subscriptionRequestRow,
                                     text: { hexFull(bytes: request.subscriberId) })
                    // Scoped container (goal-doc rule 5): `subscription-request-paid-badge`
                    // is a conditional singleton (only rows with a verified
                    // payment render it) — the flat occurrence-index heuristic
                    // can't resolve scope index N once N picks an unpaid row.
                    .automationScope(Ids.subscriptionRequestRow, index: offset)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.subscriptionRequestsSection)
        .automationValue(Ids.subscriptionRequestsSection, text: { String(vm.requests.count) })
    }

    // ── §3 Subscribers roster ───────────────────────────────────────────

    private var subscribersSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.subscriptions.subscribers).font(.headline)

            HStack {
                Text(L.subscriptions.tierSelectLabel).foregroundStyle(.secondary)
                Picker("", selection: Binding(
                    get: { vm.selectedTier },
                    set: { newValue in Task { await vm.selectTier(newValue) } }
                )) {
                    ForEach(vm.tierNames, id: \.self) { name in
                        Text(name).tag(name)
                    }
                }
                .labelsHidden()
                .accessibilityIdentifier(Ids.subscriptionSubscribersTierSelect)
                .automationSelect(Ids.subscriptionSubscribersTierSelect,
                                  value: { vm.selectedTier }) { newValue in
                    Task { await vm.selectTier(newValue) }
                }
            }

            if vm.subscribers.isEmpty {
                Text(L.subscriptions.noSubscribers)
                    .font(.caption).foregroundStyle(.secondary)
            }
            ForEach(vm.subscribers, id: \.subscriberId) { sub in
                SubscriptionSubscriberRow(subscriber: sub, vm: vm)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(Ids.subscriptionSubscriberRow)
                    .automationValue(Ids.subscriptionSubscriberRow,
                                     text: { hexFull(bytes: sub.subscriberId) })
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.subscriptionSubscribersSection)
        .automationValue(Ids.subscriptionSubscribersSection,
                         text: { String(vm.subscribers.count) })
    }

    // ── §4 Payment providers ────────────────────────────────────────────

    #if !FAUNA_EXCISE_PAYMENTS
    private var paymentProvidersSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.subscriptions.paymentProviders).font(.headline)

            Button(L.subscriptions.addProvider) { vm.openProviderForm() }
                .accessibilityIdentifier(Ids.subscriptionProviderAddButton)
                .automationActivate(Ids.subscriptionProviderAddButton) { vm.openProviderForm() }

            if vm.showProviderForm {
                providerForm
            }

            if vm.providers.isEmpty {
                Text(L.subscriptions.noProviders)
                    .font(.caption).foregroundStyle(.secondary)
            }
            ForEach(vm.providers, id: \.kind) { provider in
                SubscriptionProviderRow(provider: provider, vm: vm)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(Ids.subscriptionProviderRow)
                    .automationValue(Ids.subscriptionProviderRow, text: { provider.kind })
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.subscriptionProviderSection)
        .automationValue(Ids.subscriptionProviderSection, text: { String(vm.providers.count) })
    }

    /// The add-provider form (`subscription-provider-form`): kind select (the
    /// shared `fauna-payments` registry, rendered verbatim/unlocalized — a kind
    /// is an adapter identifier, not user copy — mirrors linux's raw
    /// `DropDown::from_strings`), the webhook-verification secret (masked,
    /// never pre-filled — the nest never echoes it back), and the entitled
    /// tier (single-select from the author's own §1 tiers).
    private var providerForm: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(L.subscriptions.providerKindLabel).foregroundStyle(.secondary)
                Picker("", selection: $vm.formProviderKind) {
                    ForEach(vm.providerKinds, id: \.self) { kind in
                        Text(kind).tag(kind)
                    }
                }
                .labelsHidden()
                .accessibilityIdentifier(Ids.subscriptionProviderFormKind)
                .automationSelect(Ids.subscriptionProviderFormKind,
                                  value: { vm.formProviderKind }) { newValue in
                    vm.formProviderKind = newValue
                }
            }

            // Read-only live preview of the exact URL to register at the
            // provider's dashboard — recomputes as the kind select changes, no
            // nest round-trip (`payments_webhook_url`, shared Rust).
            HStack {
                Text(L.subscriptions.webhookUrlLabel).foregroundStyle(.secondary)
                automationText(Ids.subscriptionProviderFormWebhookUrl, vm.providerWebhookUrl)
                    .lineLimit(1).truncationMode(.middle)
                    .frame(maxWidth: .infinity, alignment: .leading)
                CopyButton(Ids.subscriptionProviderFormWebhookUrlCopyButton,
                           text: vm.providerWebhookUrl)
            }

            HStack {
                Text(L.subscriptions.webhookSecret).foregroundStyle(.secondary)
                Spacer(minLength: 12)
                SecureField("", text: $vm.formProviderSecret)
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: 240)
                    .accessibilityIdentifier(Ids.subscriptionProviderFormSecret)
                    .automationField(Ids.subscriptionProviderFormSecret, text: $vm.formProviderSecret)
            }

            HStack {
                Text(L.subscriptions.providerTierLabel).foregroundStyle(.secondary)
                Picker("", selection: $vm.formProviderTier) {
                    ForEach(vm.tierNames, id: \.self) { name in
                        Text(name).tag(name)
                    }
                }
                .labelsHidden()
                .accessibilityIdentifier(Ids.subscriptionProviderFormTierMap)
                .automationSelect(Ids.subscriptionProviderFormTierMap,
                                  value: { vm.formProviderTier }) { newValue in
                    vm.formProviderTier = newValue
                }
            }

            HStack {
                Spacer()
                Button(L.subscriptions.cancel) { vm.cancelProviderForm() }
                    .accessibilityIdentifier(Ids.subscriptionProviderFormCancel)
                    .automationActivate(Ids.subscriptionProviderFormCancel) { vm.cancelProviderForm() }
                Button(L.subscriptions.save) { Task { await vm.saveProviderForm() } }
                    .accessibilityIdentifier(Ids.subscriptionProviderFormSave)
                    .automationActivate(Ids.subscriptionProviderFormSave) {
                        Task { await vm.saveProviderForm() }
                    }
                    // The COMMIT gates, not the buffer: the form's fields and
                    // Cancel stay live; Save issues
                    // `fauna.payments.providers.set`.
                    .faunaGate("fauna.payments.providers.set")
            }
        }
        .padding(.vertical, 6)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.subscriptionProviderForm)
        .automationValue(Ids.subscriptionProviderForm)
    }

    // ── §5 Manual claim codes ───────────────────────────────────────────

    /// The audit surface for BOTH manually- (`mintClaim`) and webhook-minted
    /// claim codes — a webhook-minted code's only other delivery channel is
    /// the provider's HTTP response body (`monetization.md` § Pillar 3).
    private var claimsSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.subscriptions.manualClaims).font(.headline)

            HStack {
                Picker("", selection: $vm.selectedClaimTier) {
                    ForEach(vm.tierNames, id: \.self) { name in
                        Text(name).tag(name)
                    }
                }
                .labelsHidden()
                .accessibilityIdentifier(Ids.subscriptionClaimTierSelect)
                .automationSelect(Ids.subscriptionClaimTierSelect,
                                  value: { vm.selectedClaimTier }) { newValue in
                    vm.selectedClaimTier = newValue
                }
                Button(L.subscriptions.mintClaim) { Task { await vm.mintClaim() } }
                    .accessibilityIdentifier(Ids.subscriptionClaimMintButton)
                    .disabled(vm.selectedClaimTier.isEmpty)
                    .automationActivate(
                        Ids.subscriptionClaimMintButton,
                        isEnabled: { !vm.selectedClaimTier.isEmpty }
                    ) { Task { await vm.mintClaim() } }
                    // The tier picker beside it is buffer and stays live.
                    .faunaGate("fauna.payments.claims.mint")
            }

            if vm.claims.isEmpty {
                Text(L.subscriptions.noClaims)
                    .font(.caption).foregroundStyle(.secondary)
            }
            ForEach(vm.claims, id: \.code) { claim in
                SubscriptionClaimRow(claim: claim)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(Ids.subscriptionClaimRow)
                    .automationValue(Ids.subscriptionClaimRow, text: { claim.code })
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.subscriptionClaimSection)
        .automationValue(Ids.subscriptionClaimSection, text: { String(vm.claims.count) })
    }
    #endif
}

// MARK: - Rows

private struct SubscriptionTierRow: View {
    let tier: FfiTierItem
    @Bindable var vm: SubscriptionsVM

    var body: some View {
        HStack(spacing: 8) {
            automationText(Ids.subscriptionTierName, tier.name)
                .frame(maxWidth: .infinity, alignment: .leading)
            automationText(Ids.subscriptionTierRank, String(tier.rank))
            automationText(Ids.subscriptionTierPrice, tier.priceHint ?? "")

            Button(L.subscriptions.edit) { vm.openEditForm(tier) }
                .accessibilityIdentifier(Ids.subscriptionTierEditButton)
                .automationActivate(Ids.subscriptionTierEditButton) { vm.openEditForm(tier) }
            Button(L.subscriptions.delete) { Task { await vm.deleteTier(tier.name) } }
                .accessibilityIdentifier(Ids.subscriptionTierDeleteButton)
                .automationActivate(Ids.subscriptionTierDeleteButton) {
                    Task { await vm.deleteTier(tier.name) }
                }
        }
    }
}

private struct SubscriptionRequestRow: View {
    let request: FfiPendingRequest
    @Bindable var vm: SubscriptionsVM

    var body: some View {
        HStack(spacing: 8) {
            automationText(Ids.subscriptionRequestSubscriber, hexFull(bytes: request.subscriberId))
                .lineLimit(1).truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading)
            automationText(Ids.subscriptionRequestTier, request.tierName)
            automationText(Ids.subscriptionRequestKind, request.kind)
            // Shown only when the request already carries a verified payment
            // (monetization.md § Pillar 3) — mirrors linux's `payment_entitled`
            // visibility gate, expressed as SwiftUI's own idiom for "absent".
            if request.paymentEntitled {
                automationText(Ids.subscriptionRequestPaidBadge, L.subscriptions.paid)
                    .font(.caption).foregroundStyle(.secondary)
            }

            Button(L.subscriptions.approve) { Task { await vm.approve(request) } }
                .accessibilityIdentifier(Ids.subscriptionRequestApproveButton)
                .automationActivate(Ids.subscriptionRequestApproveButton) {
                    Task { await vm.approve(request) }
                }
            Button(L.subscriptions.reject) { Task { await vm.reject(requestId: request.requestId) } }
                .accessibilityIdentifier(Ids.subscriptionRequestRejectButton)
                .automationActivate(Ids.subscriptionRequestRejectButton) {
                    Task { await vm.reject(requestId: request.requestId) }
                }
        }
    }
}

private struct SubscriptionSubscriberRow: View {
    let subscriber: FfiSubscriberEntry
    @Bindable var vm: SubscriptionsVM

    var body: some View {
        HStack(spacing: 8) {
            automationText(Ids.subscriptionSubscriberHandle, hexFull(bytes: subscriber.subscriberId))
                .lineLimit(1).truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button(L.subscriptions.remove) {
                Task { await vm.remove(subscriberId: subscriber.subscriberId, tierName: vm.selectedTier) }
            }
            .accessibilityIdentifier(Ids.subscriptionSubscriberRemoveButton)
            .automationActivate(Ids.subscriptionSubscriberRemoveButton) {
                Task { await vm.remove(subscriberId: subscriber.subscriberId, tierName: vm.selectedTier) }
            }
        }
    }
}

#if !FAUNA_EXCISE_PAYMENTS
private struct SubscriptionProviderRow: View {
    let provider: FfiProviderItem
    @Bindable var vm: SubscriptionsVM

    var body: some View {
        HStack(spacing: 8) {
            automationText(Ids.subscriptionProviderKind, provider.kind)
                .frame(maxWidth: .infinity, alignment: .leading)
            // The tier mapping is shown for context but carries no ui.yaml id —
            // the component's exact 4-element shape (kind/status/remove + the row
            // itself) matches linux's `build_provider_row`, which also renders the
            // tier as a plain, unregistered `Label`.
            Text(provider.tier).foregroundStyle(.secondary)
            // Shared evidence-based 3-state decision
            // (`fauna_core::format::provider_status_label`) — never an active
            // probe; see monetization.md § Pillar 3. Same idiom as linux's
            // `tiers.rs` provider row.
            automationText(Ids.subscriptionProviderStatus,
                           renderLocalizedText(providerStatusLabel(
                               lastVerifiedAt: provider.lastVerifiedAt,
                               lastRejectedAt: provider.lastRejectedAt)))
                .foregroundStyle(.secondary)

            Button(L.subscriptions.remove) { Task { await vm.removeProvider(kind: provider.kind) } }
                .accessibilityIdentifier(Ids.subscriptionProviderRemoveButton)
                .automationActivate(Ids.subscriptionProviderRemoveButton) {
                    Task { await vm.removeProvider(kind: provider.kind) }
                }
                .faunaGate("fauna.payments.providers.remove")
        }
    }
}

/// One claim-code row: bare code/tier/status labels, no captions — matches
/// the §4 provider-row idiom (linux `build_claim_row`). Status is the shared
/// 3-state decision (`claim_status_label`); redeemed wins over voided.
private struct SubscriptionClaimRow: View {
    let claim: FfiClaimItem

    var body: some View {
        HStack(spacing: 8) {
            automationText(Ids.subscriptionClaimCode, claim.code)
                .frame(maxWidth: .infinity, alignment: .leading)
            automationText(Ids.subscriptionClaimTier, claim.tier)
            automationText(Ids.subscriptionClaimStatus,
                           renderLocalizedText(claimStatusLabel(
                               redeemed: claim.redeemedBy != nil,
                               voided: claim.voidedAt != nil)))
                .foregroundStyle(.secondary)
        }
    }
}
#endif

// MARK: - Offers tab (OTHER-profile subscriber browse)

/// Another creator's offered tiers (`subscription-offers-section` /
/// `subscription-offer-list`). A dumb renderer of `ProfileOffersVM`; the free
/// "followers" tier is excluded (the header `profile-follow-button`'s job).
/// Mirrors linux `views/profile/offers.rs`.
private struct SubscriptionOffersTab: View {
    @Bindable var vm: ProfileOffersVM

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text(L.subscriptions.offers).font(.headline)

                if vm.offers.isEmpty {
                    Text(L.subscriptions.noOffers)
                        .font(.caption).foregroundStyle(.secondary)
                }
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(vm.offers, id: \.name) { offer in
                        SubscriptionOfferRow(offer: offer, vm: vm)
                            .accessibilityElement(children: .contain)
                            .accessibilityIdentifier(Ids.subscriptionOfferRow)
                            .automationValue(Ids.subscriptionOfferRow, text: { offer.name })
                    }
                }
                // The repeatable-rows component (ui.yaml `subscription-offer-list`);
                // `.contain` keeps the per-row child ids queryable under the id.
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.subscriptionOfferList)
                .automationValue(Ids.subscriptionOfferList, text: { String(vm.offers.count) })
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        // Container id + `.contain` so BOTH `is_visible("subscription-offers-
        // section")` and the per-row child ids resolve (the recurring clobber
        // guard); the value read powers is_visible.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.subscriptionOffersSection)
        .automationValue(Ids.subscriptionOffersSection, text: { String(vm.offers.count) })
    }
}

private struct SubscriptionOfferRow: View {
    let offer: FfiTierItem
    @Bindable var vm: ProfileOffersVM

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            automationText(Ids.subscriptionOfferName, offer.name)
                .font(.headline)
            automationText(Ids.subscriptionOfferPrice, offer.priceHint ?? "")
                .font(.caption).foregroundStyle(.secondary)
            automationText(Ids.subscriptionOfferDescription, offer.description ?? "")
                .font(.caption).foregroundStyle(.secondary)

            // External checkout link — only when the tier carries a payment_url
            // (mirrors linux, which omits the row otherwise).
            if let url = offer.paymentUrl, !url.isEmpty {
                Button(L.subscriptions.paymentUrl) { openPayment(url) }
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.subscriptionOfferPaymentLink)
                    .automationActivate(Ids.subscriptionOfferPaymentLink) { openPayment(url) }
            }

            HStack {
                automationText(Ids.subscriptionOfferStatus, statusText)
                    .font(.caption)
                Spacer()
                Button(L.subscriptions.subscribe) { Task { await vm.subscribe(offer.name) } }
                    .accessibilityIdentifier(Ids.subscriptionOfferSubscribeButton)
                    .automationActivate(Ids.subscriptionOfferSubscribeButton) {
                        Task { await vm.subscribe(offer.name) }
                    }
                    // The payment link beside it just hands a URL to the OS —
                    // no round trip, so it stays live (tui's `OpenPaymentLink`
                    // makes the same split).
                    .faunaGate("fauna.subscriptions.subscribe")
            }
        }
    }

    private var statusText: String {
        renderLocalizedText(offerStatusLabel(status: vm.status(for: offer.name)))
    }

    /// Refused for a non-`https` scheme via the shared `is_safe_payment_url`
    /// UniFFI export (F-CL2 anti-phishing-redirect class) — the
    /// payment_url is nest/author-supplied, so it is untrusted the same way
    /// linux's `views/profile/offers.rs` and web's `isSafeNavUrl` treat it.
    private func openPayment(_ url: String) {
        openPaymentURL(url) { vm.errorMessage = $0 }
    }
}

// MARK: - Edit form (SELF publish/edit)

/// The profile edit form (`profile-edit-form`): display name / bio / avatar /
/// banner / repeatable links → save (`build_edited_profile_with_images` sign →
/// `profile.set`). A dumb renderer of `ProfileEditVM`; `onSaved` re-reads the
/// header. Mirrors linux `views/profile/edit.rs` + android `ProfileEditForm`.
private struct ProfileEditFormView: View {
    @Bindable var vm: ProfileEditVM
    let onSaved: () async -> Void
    @State private var showAvatarImporter = false
    @State private var showBannerImporter = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            EditableFieldRow(L.profile.editDisplayName, "profile-edit-display-name", $vm.displayName,
                             labelColor: .secondary, maxWidth: 240, trailingAlign: false, rowAlignment: .center)
            EditableFieldRow(L.profile.editBio, "profile-edit-bio", $vm.bio,
                             labelColor: .secondary, maxWidth: 240, trailingAlign: false, rowAlignment: .center)

            HStack {
                imagePickerButton(
                    "profile-edit-avatar", label: L.profile.editAvatar,
                    stagedPath: vm.avatarStagedPath, isPresented: $showAvatarImporter,
                    onPicked: vm.stageAvatar)
                Button(L.profile.editRemoveAvatar) { vm.clearAvatar() }
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.profileEditAvatarRemoveButton)
                    .automationActivate(Ids.profileEditAvatarRemoveButton) { vm.clearAvatar() }
            }
            HStack {
                imagePickerButton(
                    "profile-edit-banner", label: L.profile.editBanner,
                    stagedPath: vm.bannerStagedPath, isPresented: $showBannerImporter,
                    onPicked: vm.stageBanner)
                Button(L.profile.editRemoveBanner) { vm.clearBanner() }
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.profileEditBannerRemoveButton)
                    .automationActivate(Ids.profileEditBannerRemoveButton) { vm.clearBanner() }
            }

            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(vm.links.enumerated()), id: \.offset) { index, _ in
                    HStack {
                        linkField("profile-edit-link-label", linkBinding(index, \.label))
                        linkField("profile-edit-link-url", linkBinding(index, \.uri))
                        Button(L.profile.editRemoveLink) { vm.removeLink(at: index) }
                            .controlSize(.small)
                            .accessibilityIdentifier(Ids.profileEditLinkRemoveButton)
                            .automationActivate(Ids.profileEditLinkRemoveButton) {
                                vm.removeLink(at: index)
                            }
                    }
                }
            }
            // Container id + `.contain` so both the list id and the per-row
            // field ids resolve (clobber guard).
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.profileEditLinkList)
            .automationValue(Ids.profileEditLinkList, text: { String(vm.links.count) })

            Button(L.profile.editAddLink) { vm.addLink() }
                .accessibilityIdentifier(Ids.profileEditLinkAddButton)
                .automationActivate(Ids.profileEditLinkAddButton) { vm.addLink() }

            if let error = vm.errorMessage {
                ErrorBanner(message: error)
            }

            HStack {
                Spacer()
                Button(L.profile.editCancel) { vm.cancel() }
                    .accessibilityIdentifier(Ids.profileEditCancelButton)
                    .automationActivate(Ids.profileEditCancelButton) { vm.cancel() }
                Button(L.profile.editSave) { Task { await vm.save(onSaved: onSaved) } }
                    .accessibilityIdentifier(Ids.profileEditSaveButton)
                    .automationActivate(Ids.profileEditSaveButton) {
                        Task { await vm.save(onSaved: onSaved) }
                    }
            }
        }
        .padding(.horizontal)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.profileEditForm)
        // Register the form container so the driver's wait-for/is_visible sees it
        // (it mounts/unmounts with vm.isOpen).
        .automationValue(Ids.profileEditForm)
    }

    /// A write-back binding into link row `index`'s `label`/`uri` (`FfiProfileLink`
    /// is a value type held in the VM's `links` array).
    private func linkBinding(_ index: Int, _ key: WritableKeyPath<FfiProfileLink, String>) -> Binding<String> {
        Binding(
            get: { vm.links.indices.contains(index) ? vm.links[index][keyPath: key] : "" },
            set: { if vm.links.indices.contains(index) { vm.links[index][keyPath: key] = $0 } }
        )
    }

    @ViewBuilder
    private func linkField(_ id: String, _ text: Binding<String>) -> some View {
        TextField("", text: text)
            .textFieldStyle(.roundedBorder)
            .accessibilityIdentifier(id)
            .automationField(id, text: text)
    }

    /// `profile-edit-avatar` / `profile-edit-banner` — opens a real
    /// `.fileImporter` (SwiftUI's cross-platform document picker: `NSOpenPanel`
    /// on macOS, the document browser on iOS — same idiom
    /// `ComposeAttachButton`/`CalendarListView`'s ICS import already use). The
    /// button's own label doubles as the staged path once a file is picked
    /// (mirrors linux `edit.rs`'s `avatar_btn`) — `automationValue` reads back
    /// exactly what's displayed, so `get_text` and the visible label can never
    /// diverge. No `automationActivate`: an OS-owned panel no in-process agent
    /// can drive (`ComposeAttachButton`'s same rationale) — the e2e stages
    /// through the `compose.file` state-injection command instead, which calls
    /// this same `onPicked` seam via `ProfileEditVM.stageAvatar`/`stageBanner`.
    @ViewBuilder
    private func imagePickerButton(
        _ id: String, label: String, stagedPath: String?,
        isPresented: Binding<Bool>, onPicked: @escaping (String, Data) -> Void
    ) -> some View {
        Button {
            isPresented.wrappedValue = true
        } label: {
            Text(stagedPath ?? label)
        }
        .controlSize(.small)
        .accessibilityIdentifier(id)
        .automationValue(id, text: { stagedPath ?? label })
        .fileImporter(isPresented: isPresented, allowedContentTypes: [.image]) { result in
            switch result {
            case .success(let url):
                guard url.startAccessingSecurityScopedResource() else { return }
                defer { url.stopAccessingSecurityScopedResource() }
                guard let data = try? Data(contentsOf: url) else { return }
                onPicked(url.path, data)
            case .failure(let error):
                // A user who just cancels the panel has not hit an error (mirrors
                // `ComposeAttachButton.stage`'s userCancelled guard).
                guard (error as? CocoaError)?.code != .userCancelled else { return }
                vm.errorMessage = "\(L.media.uploadFailed): \(error)"
            }
        }
    }
}
