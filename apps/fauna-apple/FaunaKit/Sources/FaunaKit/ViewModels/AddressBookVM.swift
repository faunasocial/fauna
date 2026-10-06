import Foundation

/// Drives the Contacts page's **Address Book** segment (`contacts.md` § Address
/// Book segment; `carddav-server.md` § Independent enablement) — a read-only
/// view over the CardDAV vCard store, a **separate store** from the social
/// contact graph `ContactsVM` renders. Both apple apps share this one VM
/// (priority #2), mirroring linux's `views/contacts/{address_book,
/// carddav_backend}.rs` and windows' `AddressBookModels.cs` +
/// `ContactsPage.LoadAddressbooksAsync`/`LoadCardsAsync`.
///
/// Reads ride `FfiCarddavClient.{listAddressbooks,queryCards}` (the Layer-2 FFI
/// seam over `fauna_client_carddav::CardDavClient`); there is no write path in
/// slice 4b. Auto-selects the first address book on load (windows/linux parity)
/// so the card list isn't empty on first entry.
@MainActor @Observable
public final class AddressBookVM {
    /// The actor's address books, in `list_addressbooks` order. Empty ⇒ the
    /// `no_addressbooks` empty state, not an error.
    public private(set) var addressbooks: [FfiAddressbookRow] = []
    /// The selected book's decoded vCards. Empty ⇒ the `no_cards` empty state.
    public private(set) var cards: [FfiCardRow] = []
    /// The currently-open book (`addressbook-item` selection) — `nil` before the
    /// first load or when there are no books.
    public private(set) var selectedAddressbookId: String?
    /// The card whose `card_detail` overlay is open — non-nil opens it (an
    /// inline `@State`-driven overlay, never a `.sheet` —
    /// `apple-e2e-automation.md` registration rule 3; mirrors `MediaItemDetailView`).
    public private(set) var selectedCard: FfiCardRow?
    public private(set) var isLoading = false
    public var errorMessage: String?

    private var carddav: FfiCarddavClient?
    /// The `APIClient` this VM is scoped to — the identity key. A fresh login mints a
    /// new `APIClient` (`FaunaClient.api` is a `let`), so a different instance is what
    /// signals an account switch and must drop and rebuild rather than keep serving
    /// the previous account's address books. Held strongly, so the comparison is on a
    /// live object rather than a recyclable `ObjectIdentifier`.
    ///
    /// Invariant: `carddav` (and everything loaded through it) is non-nil only if it
    /// was built over this `api` — `api` moves only after ``reset()`` has dropped it.
    private var api: APIClient?
    /// Guards the segment's lazy first fetch (web/windows/linux parity: fetched
    /// once per page visit, not on every segment toggle) — and, being the
    /// in-flight *task* rather than a `Bool`, makes a second caller **await the
    /// first run** instead of racing past it. One run **per api**: ``reset()`` and a
    /// ``configure(api:)`` for a different api drop it, so it can never hand the
    /// first account's run to the second.
    ///
    /// ⚠ A plain `loaded` flag here was a latent deep-link bug (fixed
    /// 2026-08-25). Two `.task`s call `configure`
    /// at mount: the plain one and the `search-result-item` Contact deep link,
    /// which must configure THEN locate. The flag was set synchronously *before*
    /// the `await api.carddavClient()`, so the deep-link call saw `loaded ==
    /// true` and returned while `carddav` was still `nil`; `locateCard` then hit
    /// its own `guard let carddav` and no-op'd in silence. The page still filled
    /// in behind it (the first task finished), so the symptom was a Contact
    /// search hit that navigated to a healthy-looking Address Book and simply
    /// never opened its card — with no error anywhere. Idempotence was never the
    /// problem; *completion* was, and only awaiting the same task gives it.
    private var configureTask: Task<Void, Never>?

    public init() {}

    /// Drop everything this VM holds for the account it was scoped to — the ONE
    /// canonical drop (`account-scoping.md` § The scoping taxonomy, the in-memory
    /// corollary: at the identity change itself, keyed on the identity, with no
    /// hand-listed field set at each caller). ``configure(api:)`` calls it when the api
    /// changes, and the shared view calls it on the nil-client phase of a switch or
    /// sign-out, so a field added to this VM is dropped at every site by adding it
    /// here and nowhere else.
    ///
    /// That is the personal vCards themselves — the address books, the open book's
    /// cards and the open contact — beside the client that fetched them. Retires the
    /// memoized first run too, and clears `api`, so a build or a load still suspended
    /// for the outgoing account finds its identity gone and drops its own result
    /// instead of landing it (the in-flight clause).
    public func reset() {
        configureTask?.cancel()
        configureTask = nil
        carddav = nil
        api = nil
        addressbooks = []
        cards = []
        selectedAddressbookId = nil
        selectedCard = nil
        isLoading = false
        errorMessage = nil
    }

    #if DEBUG
    /// Unit-test seam: stand in for a completed load, which a real one can't be
    /// offline (it dials the nest). Never called by production.
    func seedForTest(addressbooks: [FfiAddressbookRow], cards: [FfiCardRow]) {
        self.addressbooks = addressbooks
        self.cards = cards
        self.selectedAddressbookId = addressbooks.first?.id
    }
    #endif

    /// Vend the `FfiCarddavClient` over the shared connection, then load the
    /// book list. Safe to call from several `.task`s at once: the first call
    /// does the work and every concurrent caller awaits that same run, so a
    /// caller that continues afterwards is guaranteed a configured `carddav`.
    ///
    /// One run per `APIClient`: a DIFFERENT api (a re-login mints a fresh one) is
    /// another account, so the previous account's state is dropped via ``reset()``
    /// **before** the new run starts — never the first account's memoized run handed
    /// to the second, which is what left account A's address books on account B's
    /// page.
    public func configure(api: APIClient) async {
        if let current = self.api, current !== api { reset() }
        self.api = api
        if let configureTask {
            await configureTask.value
            return
        }
        let task = Task { @MainActor [weak self] in
            guard let self else { return }
            let built: FfiCarddavClient
            do {
                built = try await api.carddavClient()
            } catch {
                // The build suspended: a `reset()` (the switch's nil-client phase) or
                // a `configure` for another api may have run meanwhile, and a failure
                // for the outgoing account must not paint on that account's successor.
                guard self.api === api else { return }
                self.errorMessage = DisplayError.message(error)
                return
            }
            guard self.api === api else { return }
            self.carddav = built
            await self.loadAddressbooks()
        }
        configureTask = task
        await task.value
    }

    /// Refresh the book list (`fauna.bridges.list_addressbooks`), then
    /// auto-open the first book so the card list isn't empty on entry (windows
    /// `LoadAddressbooksAsync` / linux `update_book_list` parity).
    ///
    /// Every read below re-checks, after its `await`, that `carddav` is still the
    /// client it started on: a `reset()` or a switch that ran meanwhile means the
    /// answer is the OUTGOING account's, and must not land on the next account.
    public func loadAddressbooks() async {
        guard let carddav else { return }
        isLoading = true
        errorMessage = nil
        // Only the run that still owns the page clears the spinner — a superseded
        // run's `defer` would otherwise switch off the NEXT account's.
        defer { if self.carddav === carddav { isLoading = false } }
        let books: [FfiAddressbookRow]
        do {
            books = try await carddav.listAddressbooks()
        } catch {
            guard self.carddav === carddav else { return }
            errorMessage = DisplayError.message(error)
            return
        }
        guard self.carddav === carddav else { return }
        addressbooks = books
        if let first = books.first {
            await selectAddressbook(first.id)
        } else {
            cards = []
            selectedAddressbookId = nil
        }
    }

    /// Open `addressbookIdHex` in the picker and load its cards
    /// (`fauna.bridges.query_cards`).
    public func selectAddressbook(_ addressbookIdHex: String) async {
        guard let carddav else { return }
        selectedAddressbookId = addressbookIdHex
        isLoading = true
        errorMessage = nil
        defer { if self.carddav === carddav { isLoading = false } }
        do {
            let loaded = try await carddav.queryCards(addressbookIdHex: addressbookIdHex)
            guard self.carddav === carddav else { return }
            cards = loaded
        } catch {
            guard self.carddav === carddav else { return }
            errorMessage = DisplayError.message(error)
        }
    }

    /// Open the `card_detail` overlay for a tapped `vcard-card`.
    public func selectCard(_ card: FfiCardRow) {
        selectedCard = card
    }

    /// Deep-link door for a `search-result-item` Contact-arm activation
    /// (`ui/search.md` § Where logic lives → Result navigation (deep link)):
    /// locate a card by its `uid_hash` — the identity that survives an
    /// in-place vCard edit, unlike the server-assigned `card_id` the
    /// picker/list otherwise key on — via the shared
    /// `FfiCarddavClient.locateCardByUidHash`, never a client-side scan or a
    /// re-derived join. Selects the holding book and opens its `card_detail`
    /// overlay from the FOUND book's own cards (never re-querying), matching
    /// the linux reference's switch-first-then-locate ordering. Returns
    /// whether a card was found; a stale/deleted/inaccessible uid_hash
    /// degrades to a no-op, the same posture every other stale-id open takes.
    @discardableResult
    public func locateCard(uidHash: String) async -> Bool {
        guard let carddav else { return false }
        isLoading = true
        errorMessage = nil
        defer { if self.carddav === carddav { isLoading = false } }
        do {
            let located = try await carddav.locateCardByUidHash(uidHashHex: uidHash)
            guard self.carddav === carddav else { return false }  // outgoing account's answer — see `loadAddressbooks`
            // The picker's rows land either way — the read returned them, and a
            // deep link arrives on a page the user may never have opened, so
            // leaving `addressbooks` empty would strand them on a blank picker
            // the moment they close the card (tui's `Outcome::CardLocated`).
            addressbooks = located.books
            guard let found = located.found else {
                // No book holds that `uid_hash` any more: the card was deleted
                // between being indexed and being clicked. Say so on
                // `error-message` — the DROPPED outcome `ui/search.md` § Where
                // logic lives → Result navigation (deep link) requires, and a
                // silently-unchanged page would read as a dead row. tui does
                // exactly this (`CARD_NOT_FOUND`); macOS was degrading in
                // silence.
                errorMessage = L.contacts.addressBook.cardNotFound
                return false
            }
            selectedAddressbookId = found.addressbookId
            cards = found.cards
            guard let card = found.cards.first(where: { $0.id == found.cardId }) else {
                // The book was located but its own `card_id` is not among the
                // cards it returned — an internal inconsistency, not a stale
                // link. Same user-facing outcome; never a silent no-op.
                errorMessage = L.contacts.addressBook.cardNotFound
                return false
            }
            selectedCard = card
            return true
        } catch {
            guard self.carddav === carddav else { return false }
            errorMessage = DisplayError.message(error)
            return false
        }
    }

    /// Close the `card_detail` overlay.
    public func dismissCardDetail() {
        selectedCard = nil
    }

    /// Re-pull on a `fauna.addressbook.changed` push (`FaunaClient`'s
    /// `.onAddressBookChanged`) — the apple twin of android's
    /// `ContactsVM.refreshAddressBook` (`ApiClient.addressBookChangedTick`).
    /// Unlike ``loadAddressbooks()`` this keeps the OPEN book open rather than
    /// resetting to the first one: a push mid-visit must not yank the user off
    /// the book they're looking at. Re-reading the open book's cards can race a
    /// mid-refresh tap to a DIFFERENT book, so — beside the ordinary account-switch
    /// guard every read here already carries — it re-checks `selectedAddressbookId`
    /// after the query lands and drops a reply for a book the user has since left.
    public func refreshFromPush() async {
        guard let carddav else { return }
        let books: [FfiAddressbookRow]
        do {
            books = try await carddav.listAddressbooks()
        } catch {
            guard self.carddav === carddav else { return }
            errorMessage = DisplayError.message(error)
            return
        }
        guard self.carddav === carddav else { return }
        addressbooks = books
        guard let openId = selectedAddressbookId, books.contains(where: { $0.id == openId }) else {
            // Nothing was open, or the open book itself vanished — fall back to
            // the ordinary auto-select-first behavior.
            if let first = books.first {
                await selectAddressbook(first.id)
            } else {
                cards = []
                selectedAddressbookId = nil
            }
            return
        }
        do {
            let loaded = try await carddav.queryCards(addressbookIdHex: openId)
            // The book the user has open may have changed while this awaited —
            // a reply for the book they've since left must not clobber the one
            // now showing.
            guard self.carddav === carddav, selectedAddressbookId == openId else { return }
            cards = loaded
        } catch {
            guard self.carddav === carddav, selectedAddressbookId == openId else { return }
            errorMessage = DisplayError.message(error)
        }
    }

    /// `ORG` components joined with " · " (empty components dropped) — byte-
    /// identical to the linux/windows/web render (no shared-Rust join exists;
    /// each app mirrors the same trivial one-liner, matching established
    /// prior art rather than adding an FFI boundary for it).
    public nonisolated static func orgLabel(_ org: [String]) -> String {
        org.filter { !$0.isEmpty }.joined(separator: " · ")
    }
}
