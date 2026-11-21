// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// Shared view-model for the admin `admin-web` page (macOS + iOS, one FaunaKit VM).
/// Drives the **apex-actor picker**: an Admin-class
/// designation of which actor's `web` content serves at `https://<domain>/`,
/// "None" clearing it to the built-in info page. The direct analogue of the
/// per-domain catch-all mail actor, and built the same way: the actor list is
/// every account on the nest, read through the shared
/// `fauna_client_admin::users_list_all` (`admin.md` § 2 → *Which accounts a
/// picker offers*); the current designation + set/clear ride
/// the shared `FfiWebClient` (`fauna.web.set/get_apex_actor`). The picker model
/// (option 0 = "None"; a current designation not among the loaded actors gets a
/// trailing entry so it stays visible + selected) mirrors linux
/// `apps/fauna-linux/src/settings/admin_web.rs` + web `admin/web/+page.svelte`.
/// Non-optimistic: state is set only from nest-confirmed reads. Target:
/// `docs/goal/behavior/web-content-hosting.md` § Admin apex hosting.
@MainActor @Observable
public final class AdminWebVM {
    /// One pickable option. `id` is the stable index = the SwiftUI `Picker` tag
    /// (index-based dispatch so a label collision can't mis-route, like linux/web).
    public struct ApexOption: Identifiable, Equatable {
        public let id: Int
        public let actorId: Data?  // nil = "None" (clear)
        public let label: String
    }

    /// The apex-picker options (index 0 = "None").
    public private(set) var options: [ApexOption] = []
    /// The currently-selected option index (the designated apex actor, or 0).
    public private(set) var selectedIndex: Int = 0
    /// The `admin-web-apex-info` line, from the shared
    /// `fauna_client_web::apex_info_text`: where the apex serves, or why there
    /// is no address — never a composed `https:///` from an empty serving
    /// domain (`admin.md` § 7 Web).
    public private(set) var apexInfo: String = ""
    /// Where the apex read (`fauna.web.get_apex_actor`) stands. The picker is
    /// not live before it lands and stays blocked (*failed to load*) after it
    /// failed (`admin.md` § 7 Web).
    public private(set) var read = PageRead()
    /// Whether the read this page's read-gated controls wait for has resolved
    /// (``AdminPageReading``): the page's own read. An unanswered one is asked again when the
    /// nest connection comes back.
    public var readAnswered: Bool { read.state == .resolved }
    /// Page-level error surface (`error-message`).
    public var errorMessage: String?
    public private(set) var isLoading = false

    private var web: FfiWebClient?
    private var admin: FfiAdminClient?
    private var domain: String = ""

    public init() {}

    /// `admin-web-apex-actor-select`'s answer: the apex read's own
    /// (`Possibility::from_read`, UniFFI `read_possibility`).
    public var apexPickerAnswer: Possibility { read.possibility }

    /// Vend the web + admin clients from APIClient and load the current
    /// designation + actor list. Idempotent (clients built once).
    public func configure(api: APIClient) async {
        if web == nil {
            do {
                web = try await api.webClient()
                admin = try await api.adminClient()
            } catch { errorMessage = DisplayError.message(error); return }
        }
        await hydrate()
    }

    /// Re-read the nest's serving domain, the apex designation and the
    /// pickable actors, then rebuild the model. The domain is re-read every
    /// hydrate rather than cached once — the same "resolved fresh on every
    /// read" rule `WebPublishStore` follows — so the apex explainer can never
    /// answer off a stale value.
    ///
    /// The reads are not equally fatal (tui's `load_web_snapshot`): the apex
    /// read failing leaves the page nothing true to show, so the picker is
    /// blocked and the error paints; the account list and the serving domain
    /// degrade rather than blank the page — the designation still shows — but
    /// neither failure is silent: each reaches `error-message` (`admin.md`
    /// § 7 Web, `ui/README.md` § Offer only what is possible, rule 7).
    public func hydrate() async {
        guard let web, let admin else { return }
        isLoading = true
        defer { isLoading = false }
        let current: Data?
        do {
            current = try await web.getApexActor()
        } catch {
            read.failed()
            errorMessage = DisplayError.message(error)
            return
        }
        var failures: [String] = []
        let users: [FfiAdminUser]
        do {
            users = try await admin.usersListAll()
        } catch {
            users = []
            if let text = DisplayError.message(error) { failures.append(text) }
        }
        do {
            domain = try await web.servingDomain()
        } catch {
            domain = ""
            if let text = DisplayError.message(error) { failures.append(text) }
        }
        apply(current: current, users: users)
        read.resolved()
        if !failures.isEmpty { errorMessage = failures.joined(separator: "; ") }
    }

    /// Select the option at `index` — dispatch set (`Some`) / clear (`None`), then
    /// re-hydrate from the nest's confirmed state.
    public func select(index: Int) async {
        // The pick asks what the paint asked: refused with its reason on
        // `error-message` before the read lands or after it failed, never a
        // silent return (`admin.md` § 7 Web).
        if refusePress(apexPickerAnswer, errorMessage: &errorMessage) { return }
        guard index >= 0, index < options.count else { return }
        guard let web else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            _ = try await web.setApexActor(actorId: options[index].actorId)
            await hydrate()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// Build the picker model + apex URL from the current designation + the loaded
    /// actors (mirrors linux `admin_web.rs::render`). Not `private` — a pure
    /// function of its arguments, so a unit test drives it directly without a
    /// nest round trip (same shape as `AdminDnsVM.actorLabel`'s injectivity pin).
    func apply(current: Data?, users: [FfiAdminUser]) {
        errorMessage = nil
        apexInfo = renderLocalizedText(webApexInfoText(domain: domain))

        var opts: [ApexOption] = [ApexOption(id: 0, actorId: nil, label: L.admin.webPage.apexNone)]
        for u in users {
            opts.append(ApexOption(id: opts.count, actorId: u.actorId, label: adminPickerOption(user: u)))
        }
        var selected = 0
        if let current {
            if let i = users.firstIndex(where: { $0.actorId == current }) {
                selected = i + 1
            } else {
                // Designation paginated out / not in the list — keep it visible.
                let full = hexFull(bytes: current)
                opts.append(ApexOption(id: opts.count, actorId: current, label: L.admin.actorIdFallbackLabel(short: full)))
                selected = opts.count - 1
            }
        }
        options = opts
        selectedIndex = selected
    }
}
#endif // !FAUNA_KIDS
