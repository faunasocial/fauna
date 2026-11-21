// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// Shared view-model for the admin `admin-aliases` external-forwarders page
/// (macOS + iOS, one FaunaKit VM). A thin proxy over the
/// shared `ForwarderMachine` (UniFFI): the machine owns the forwarder list +
/// hosted-domain feed + create/delete orchestration (hex-decoding the alias id,
/// re-reading after each mutation); this VM holds the latest `ForwardersSnapshot`
/// as `@Observable` state and re-reads it after each `hydrate` / `dispatch`. The
/// machine is **pull-based** (no observer callback, like `MailSpamMachine` /
/// `BridgeApprovalMachine`), so re-assigning `snapshot` drives the SwiftUI
/// re-render.
///
/// Target behavior: `docs/goal/behavior/admin.md` § 4 Aliases (external
/// forwarders, Kind 7) + `mail-aliases.md` § Kind 7. Reference renderers: linux
/// (`apps/fauna-linux/src/views/admin.rs::build_admin_aliases_page`), windows
/// (`AdminAliasesViewModel`), web (`routes/admin/aliases/+page.svelte`).
/// This page owns ONLY the external-forwarder slice; the per-domain catch-all
/// (Kind 4) lives on `admin-dns`.
@MainActor @Observable
public final class ForwardersVM {
    /// Latest snapshot; `nil` until `configure`. The view reads `forwarders`
    /// (the rows) + `localDomains` (the add-form domain picker) + `status`.
    public private(set) var snapshot: ForwardersSnapshot?
    /// Page-level connect/build error (`error-message`).
    public var errorMessage: String?
    /// Alias-action error (`admin-aliases-action-error`) — the machine's
    /// `snapshot.error` from a failed create/list/delete (mirrors AdminVM's
    /// `actionError`).
    public var actionError: String?
    public private(set) var isLoading = false

    /// The forwarder add form's answer — the shared
    /// `ForwarderMachine::add_possibility` (admin.md § Errors & edge cases,
    /// *Controls wait for their read*): blocked until the forwarder and
    /// local-domain read resolves (the domain choice comes from it), blocked
    /// after a failed one, blocked while a change is on its way.
    public private(set) var addAnswer: Possibility = readPossibility(read: .pending)
    /// The add form's picker and fields — the shared
    /// `ForwarderMachine::form_possibility` (admin.md § 4, *A forwarder needs a
    /// hosted domain*): [`addAnswer`], then blocked with the reason naming DNS
    /// while the nest hosts no local domain.
    public private(set) var formAnswer: Possibility = readPossibility(read: .pending)
    /// Where the list read stands — the empty state is painted only once it
    /// resolved (an unread list is not an empty one).
    public private(set) var listRead: ReadState = .pending
    /// Whether the read this page's read-gated controls wait for has resolved
    /// (``AdminPageReading``): the forwarder list read (`ForwarderMachine::read_state`). An unanswered one is asked again when the
    /// nest connection comes back.
    public var readAnswered: Bool { listRead == .resolved }

    private var machine: ForwarderMachine?
    private var buildFailed = false

    public init() {}

    /// The forwarder rows to render (each row's `address` is pre-composed
    /// `<pattern>@<localDomain>`).
    public var forwarders: [ForwarderView] { snapshot?.forwarders ?? [] }
    /// Hosted local domains for the add-form picker.
    public var localDomains: [String] { snapshot?.localDomains ?? [] }
    /// True while a list/create/delete round-trip is in flight — gates the form.
    public var isBusy: Bool { isLoading || snapshot?.status == .working }

    /// Vend the machine from APIClient and load the first feed. Idempotent.
    public func configure(api: APIClient) async {
        guard machine == nil else { return }
        let built: ForwarderMachine
        do {
            built = try await api.forwardersMachine()
        } catch {
            // A concurrent build that succeeded owns the page; this one's
            // failure is not the page's.
            if machine == nil {
                errorMessage = DisplayError.message(error)
                buildFailed = true
                addAnswer = readPossibility(read: .failed)
                formAnswer = addAnswer
                listRead = .failed
            }
            return
        }
        // Re-entrant `configure`: the build that finishes first wins; a later
        // one is dropped, never replaces it (same shape as `LabelerCatalogVM`).
        guard machine == nil else { return }
        machine = built
        buildFailed = false
        apply(built)
        await hydrate()
    }

    /// Re-read the forwarder list + hosted domains (page mount / refresh).
    public func hydrate() async {
        guard let machine else { return }
        isLoading = true
        defer { isLoading = false }
        do { try await machine.hydrate() }
        catch { errorMessage = DisplayError.message(error) }
        apply(machine)
    }

    /// Whether the page offers `admin-aliases-forwarder-add-domains-step` — the
    /// one step that unblocks the form: exactly while the form is blocked for
    /// want of a hosted domain (a pending or failed read gives another reason,
    /// and offers no step).
    public var offersDomainsStep: Bool {
        if case .blocked(let reason) = formAnswer {
            return reason.key == Self.noHostedDomain
        }
        return false
    }

    /// The blocked-reason key `ForwarderMachine::form_possibility` gives with
    /// no hosted local domain (`fauna_client_mail_settings::forwarders::NO_HOSTED_DOMAIN`).
    static let noHostedDomain = "admin.aliases_page.no_hosted_domain"

    /// The submit's answer for this draft: the shared
    /// `ForwarderMachine::create_possibility` over the picked domain (the read,
    /// a change on its way, a hosted domain picked), then the draft's own
    /// `forwarder_create_draft_possibility` (a non-blank local part and
    /// destination) — so a page still loading says so first.
    public func createAnswer(localDomain: String, pattern: String, forwardTarget: String) -> Possibility {
        machinePossibility(machine, buildFailed: buildFailed) {
            $0.createPossibility(localDomain: localDomain)
        }
        .and(forwarderCreateDraftPossibility(
            localDomain: localDomain, pattern: pattern, forwardTarget: forwardTarget))
    }

    /// Create an external forwarder `<pattern>@<localDomain>` → `forwardTarget`.
    /// The machine re-reads the list (so the new row appears) on success.
    public func create(localDomain: String, pattern: String, forwardTarget: String) async {
        // The press asks the paint's answer again, so a press racing the read,
        // a create still on its way or an empty `local_domain` is refused with
        // its reason, never sent.
        if refusePress(createAnswer(localDomain: localDomain, pattern: pattern,
                                    forwardTarget: forwardTarget),
                       errorMessage: &errorMessage) { return }
        await dispatch(.create(localDomain: localDomain, pattern: pattern, forwardTarget: forwardTarget))
    }

    /// Delete the forwarder identified by its hex alias id. The machine re-reads.
    public func delete(aliasIdHex: String) async {
        await dispatch(.delete(aliasIdHex: aliasIdHex))
    }

    private func dispatch(_ action: ForwarderAction) async {
        guard let machine else { return }
        isLoading = true
        defer { isLoading = false }
        try? await machine.dispatch(action: action)
        apply(machine)
    }

    /// Re-read the snapshot and the answers painted from it.
    private func apply(_ machine: ForwarderMachine) {
        let snap = machine.snapshot()
        snapshot = snap
        actionError = snap.error
        addAnswer = machine.addPossibility()
        formAnswer = machine.formPossibility()
        listRead = machine.readState()
    }
}
#endif // !FAUNA_KIDS
