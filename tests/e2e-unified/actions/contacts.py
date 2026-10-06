from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class ContactsActions:
    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        """Navigate to the contacts list."""
        self.driver.navigate_to("contacts")

    def require_address_book_supported(self) -> None:
        """Skip unless this app's `enable_mail_plain` seeded-read path for the
        Address Book segment is confirmed (e2e convention 7 — the platform
        check lives in the action layer, not the test body).

        linux + windows + apple + tui drive the scripted CardDAV MUA seed via a
        KNOWN bridge password (`enable_mail_plain`); web joined too — its own `enable_mail_plain` path is proven
        directly by `test_mail_credentials.py`'s web marker. android is the
        one still owed (route-3 follow-on, per `test_events`).
        """
        driver = self.driver
        if not (
            driver.is_linux()
            or driver.is_windows()
            or driver.is_macos()
            or driver.is_ios()
            or driver.is_tui()
            or driver.is_web()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                driver,
                surface="the enable_mail_plain seeded CardDAV-read path",
                detail="enabling mail with a KNOWN bridge password is wired for "
                       "linux + windows + apple + tui; web + android join once "
                       "their enable-mail-plain e2e is confirmed",
                tracked="route-3 follow-on, per test_events",
            )

    def require_address_book_live_refresh(self) -> None:
        """Skip unless this app re-reads its open Address Book on the
        ``fauna.addressbook.changed`` push — convention 7.

        The shared classifier folds the kind to ``StaleSurfaces::address_book``;
        tui (``apply_resync``), linux (``apply_stale``), web (the contacts
        page's ``onPushEvent``), macos + ios (``FaunaClient.startPushObserver``
        → ``.onAddressBookChanged`` → ``AddressBookVM.refreshFromPush``) and now
        windows (``NestRpcClient.DispatchPush`` → ``AddressBookPushChanged`` →
        ``ContactsPage``) re-read the books and the open book's cards off it.
        The UniFFI apps receive the push typed
        (``FfiPushEvent::AddressBookChanged``) and android's page is wired;
        android's run stays parked behind the host emulator — parity debt
        (``skip_unbuilt``), not an absence.
        """
        if not (
            self.driver.is_tui()
            or self.driver.is_linux()
            or self.driver.is_web()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_windows()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the Address Book's live refresh on fauna.addressbook.changed",
                detail=(
                    "tui, linux, web, macos, ios and windows re-read the open "
                    "book off StaleSurfaces::address_book; android's run is "
                    "emulator-parked"
                ),
                tracked="transport.md § Push events (fauna.addressbook.changed); the cross-app lift",
            )

    def open_contact_profile(self, actor_id_hex: str, timeout: float = 10.0) -> None:
        """Open another actor's profile by tapping their contact row.

        The contact ``ListBoxRow`` carries the peer's actor id as its widget name
        (``app.rs`` reads it in ``connect_row_activated`` → ``open_profile(Some
        (hex))`` — profile.md § Relationship to Contacts), so the row resolves by
        that id. Navigates to contacts first so the row is realized (the list
        hydrates async via ``fauna.contacts.list``) then activates it.

        A missing row has two causes the row's own snapshot cannot tell apart —
        the roster read failed (the page keeps its old list and shows
        ``error-message``) or it succeeded without this actor — so the timeout
        carries the page error and the rendered roster too (e2e rule 6)."""
        self.navigate()
        try:
            self.driver.wait_for(actor_id_hex, timeout=timeout)
        except TimeoutError as e:
            raise TimeoutError(
                f"{e}; {self.driver.diagnose('error-message')}, "
                f"{self.driver.diagnose('contact-name')}"
            ) from e
        self.driver.click(actor_id_hex)

    def contact_names(self) -> list[str]:
        """Return all visible contact names."""
        count = self.driver.count("contact-name")
        return [
            self.driver.get_text("contact-name", index=i)
            for i in range(count)
        ]

    def contact_count(self) -> int:
        return self.driver.count("contact-name")

    def contact_status(self, index: int = 0) -> str:
        """Get the status text of a contact by index."""
        return self.driver.get_text("contact-status", index=index)

    def contact_statuses(self) -> list[str]:
        """Every roster row's ``contact-status`` text, in row order."""
        return [
            self.driver.get_text("contact-status", index=i)
            for i in range(self.driver.count("contact-status"))
        ]

    def confirm_count(self) -> int:
        """How many ``contact-confirm`` affordances the roster offers."""
        return self.driver.count("contact-confirm")

    def confirm_contact(self, index: int = 0) -> None:
        """Click ``contact-confirm[index]`` without settling.

        The confirm is a round trip and the roster re-reads after it, so the
        caller deadline-polls the state it expects (the row's status label, the
        affordance's disappearance) rather than trusting a fixed delay
        (convention 14)."""
        self.driver.click("contact-confirm", index=index)

    def narrow_roster(self, query: str) -> None:
        """Type ``query`` into the local roster filter, without settling — the
        latency-independent twin of :meth:`search`.

        The filter is the shared ``contact_matches_filter`` over handle, domain
        and actor id, so a peer's full actor-id hex narrows the roster to exactly
        that contact's row: ``contact-status[0]`` and ``contact-confirm[0]`` then
        name that person, whatever else the session's actor has accumulated. The
        caller polls for the narrowed state."""
        self.driver.clear_and_type("contacts-search-field", query)

    def row_names(self, index: int = 0) -> tuple[str, str, str]:
        """``(contact-name, contact-public-name, contact-labels)`` of roster row
        ``index`` — ``""`` for a secondary line the row does not render (no
        nickname, no labels). ``contact-name`` is flat-indexed; the two
        secondary lines are scoped within ``contact-row[index]``
        (`contacts.md` § The private overlay → *Where the nickname paints*)."""
        scope = f"contact-row[{index}]"

        def scoped(element_id: str) -> str:
            if self.driver.is_absent(element_id, scope=scope):
                return ""
            return self.driver.get_text(element_id, scope=scope)

        return (
            self.driver.get_text("contact-name", index=index),
            scoped("contact-public-name"),
            scoped("contact-labels"),
        )

    def find_by_actor_id(self, actor_id: str) -> None:
        """Look up a user by actor ID."""
        self.driver.clear_and_type("contact-actor-id-field", actor_id)
        self.driver.click("contact-actor-id-lookup")
        time.sleep(1)

    def require_cross_nest_knock(self) -> None:
        """Skip unless this app has built the cross-nest Find User + knock leg —
        convention 7, the platform check lives in the action layer.

        tui shipped it as the lead app (`testing.md` § Default app and nest
        mode: rust → nest → tui → batched trickle-down). The transport half is
        already shared on all seven — every app's `fauna.inbox.send` binding
        takes `recipient_nest_url`, and the decision + URL derivation are shared
        Rust (`fauna_core::resolve::is_foreign_handle_domain`,
        `fauna_provisioning::probe::peer_nest_url`) — so what the other six owe
        is the page wiring, not a mechanism. Parity debt, hence `skip_unbuilt`
        rather than `declared_absence`.
        """
        if not self.driver.is_tui():
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the cross-nest Find User + knock leg",
                detail=(
                    "a typed foreign `user@domain` resolves against the peer "
                    "nest and the knock carries `recipient_nest_url`; tui "
                    "shipped it as the lead app over the shared decision + URL "
                    "helpers, the other six still send null"
                ),
                tracked="contacts.md § Implementation status today; the batched per-app trickle-down",
            )

    def find_by_handle(self, handle: str) -> None:
        """Look a user up by a ``localpart@domain`` handle, without settling.

        The same two gestures as :meth:`find_by_actor_id`, and deliberately
        without its trailing one-second settle: a handle lookup is a real nest
        round trip — two of them when the domain is foreign (same-nest
        ``fauna.actor.by_handle``, then an anonymous one against the peer) — so a
        fixed delay is either a waste or a lie depending on the box's load
        (convention 14). The caller deadline-polls for the state this produces:
        ``contact-actor-id-result`` on a hit, ``contact-find-error`` on a miss.

        Kept separate from ``find_by_actor_id`` rather than replacing it: a raw
        actor id resolves OFFLINE (``classify_recipient`` short-circuits before
        any nest hop), so those two callers are asserting about different
        machinery even though they type into the same field.
        """
        self.driver.clear_and_type("contact-actor-id-field", handle)
        self.driver.click("contact-actor-id-lookup")

    def add_contact_no_settle(self) -> None:
        """Click ``contacts-add-button`` without settling — the latency-independent
        twin of :meth:`add_contact`.

        The knock send is one round trip same-nest and two cross-nest (the home
        nest originates ``fauna.federation.inbox.deliver`` to the peer and waits
        on it), so the one-second settle that suffices for the former is a
        coin-flip for the latter. The caller polls the recipient's own knock
        queue instead, which is the effect it actually cares about.
        """
        self.driver.click("contacts-add-button")

    def actor_id_result_text(self) -> str:
        if not self.driver.is_visible("contact-actor-id-result"):
            return ""
        return self.driver.get_text("contact-actor-id-result")

    def find_error_text(self) -> str:
        """Get the error text from a failed contact lookup."""
        if not self.driver.is_visible("contact-find-error"):
            return ""
        return self.driver.get_text("contact-find-error")

    def copy_actor_id(self, index: int = 0) -> None:
        """Click the copy-actor-id button for a contact."""
        self.driver.click("contact-actor-id-copy-btn", index=index)

    # --- The supervised ward's in-app ask (family-safety.md
    #     § Child-initiated contact requests). Both are CONDITIONAL and mutually
    #     exclusive: the button reveals only after the nest refused the send with
    #     the typed guardian-approval error, and the pending label replaces it
    #     once an ask is outstanding (read from the ward's own
    #     status.contact_requests, so it survives a restart).

    def require_contact_request_ask(self) -> None:
        """Skip unless this app builds the ward-side ask — convention 7, the
        platform check lives in the action layer.

        tui shipped it 2026-08-14 as the lead app; macOS + iOS (through the
        shared FaunaKit ``ContactAskRow``), linux and web lifted it 2026-09-26;
        the remaining apps lift it (parity debt, hence skip_unbuilt rather than
        declared_absence)."""
        if not (
            self.driver.is_tui()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_linux()
            or self.driver.is_web()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="contact-request-guardian-button",
                detail=(
                    "the ward's in-app 'ask your guardian'; tui shipped it "
                    "2026-08-14 over the shared refusal predicate, macOS, iOS, "
                    "linux and web 2026-09-26"
                ),
                tracked="family-safety.md § Child-initiated contact requests → App affordance",
            )

    def guardian_ask_offered(self) -> bool:
        return self.driver.count("contact-request-guardian-button") > 0

    def ask_guardian(self) -> None:
        """Send the ask. No settle-sleep: the click is acked, and the state it
        produces (`contact-request-pending`) is what the caller deadline-polls
        for — convention 14, assert latency-independent state."""
        self.driver.click("contact-request-guardian-button")

    def contact_request_pending_text(self) -> str:
        if not self.driver.is_visible("contact-request-pending"):
            return ""
        return self.driver.get_text("contact-request-pending")

    # --- Search ---

    def search(self, query: str) -> None:
        """Search contacts by handle."""
        self.driver.clear_and_type("contacts-search-field", query)
        time.sleep(0.5)

    def search_result_count(self) -> int:
        """Return the number of add-contact lookup results."""
        return self.driver.count("contact-actor-id-result")

    def add_contact(self) -> None:
        """Click the add-contact button after a search."""
        self.driver.click("contacts-add-button")
        time.sleep(1)

    # --- Knocks (contact requests) ---

    def knock_count(self) -> int:
        """Return the number of pending knock/contact requests."""
        return self.driver.count("knock-card")

    def knock_sender(self, index: int = 0) -> str:
        """Get the sender text of a knock at the given index."""
        return self.driver.get_text("knock-sender", index=index)

    def knock_senders(self) -> list[str]:
        """Every pending knock's ``knock-sender`` text, in row order."""
        return [
            self.driver.get_text("knock-sender", index=i)
            for i in range(self.driver.count("knock-sender"))
        ]

    def knock_index_for(self, sender_hex: str) -> int | None:
        """The knock row whose sender is ``sender_hex``, or ``None``.

        ``knock-sender`` renders the shared ``fauna_core::format::short_id`` (12
        hex characters and an ellipsis — ``value-formatting.md`` § Short id), so
        the match is on that form; ``test_knock_sender_display.py`` pins that
        every app renders exactly it."""
        short = sender_hex if len(sender_hex) <= 12 else sender_hex[:12] + "…"
        for i, text in enumerate(self.knock_senders()):
            if text == short:
                return i
        return None

    # The three knock verbs click and return. Each is a round trip after which
    # the page re-reads both lists, so the caller deadline-polls the state it
    # expects instead of trusting a fixed delay (convention 14).

    def accept_knock(self, index: int = 0) -> None:
        """Accept a knock/contact request."""
        self.driver.click("contacts-accept-button", index=index)

    def dismiss_knock(self, index: int = 0) -> None:
        """Dismiss a knock/contact request."""
        self.driver.click("knock-dismiss", index=index)

    def block_knock(self, index: int = 0) -> None:
        """Block a knock sender."""
        self.driver.click("contacts-block-button", index=index)

    # --- Address Book segment (CardDAV vCards — slice 4b) ---
    #
    # The Contacts page carries a top-level `Contacts | Address Book` segment
    # toggle (carddav-server.md § Independent enablement, contacts.md § Layout &
    # flow). The Address Book view mirrors the Events page's master-detail: an
    # address-book picker (addressbook-item) + a card list (vcard-card /
    # vcard-card-fn) + a card-detail pane (vcard-detail-*). Read-only in slice 4b.

    def switch_to_address_book(self) -> None:
        """Switch the Contacts page to the Address Book segment."""
        self.driver.wait_for("contacts-segment-addressbook", timeout=10.0)
        self.driver.click("contacts-segment-addressbook")

    def switch_to_people(self) -> None:
        """Switch the Contacts page back to the social Contacts segment."""
        self.driver.click("contacts-segment-people")

    def addressbook_count(self) -> int:
        return self.driver.count("addressbook-item")

    def wait_for_addressbook(self, timeout: float = 15.0) -> bool:
        """Poll until at least one address book renders (the read is async)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.count("addressbook-item") > 0:
                return True
            time.sleep(0.5)
        return False

    def select_addressbook(self, index: int = 0) -> None:
        """Open an address book in the picker (loads its card list)."""
        self.driver.click("addressbook-item", index=index)

    def card_count(self) -> int:
        return self.driver.count("vcard-card")

    def card_names(self) -> list[str]:
        """Return the formatted name (FN) of every vCard in the current book."""
        count = self.driver.count("vcard-card-fn")
        return [self.driver.get_text("vcard-card-fn", index=i) for i in range(count)]

    def wait_for_card(self, fn: str, timeout: float = 15.0) -> bool:
        """Poll until a vCard whose FN contains ``fn`` appears in the card list."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if any(fn in n for n in self.card_names()):
                return True
            time.sleep(0.5)
        return False

    def open_card_by_name(self, fn: str) -> None:
        """Open the card-detail pane for the vCard whose FN contains ``fn``."""
        count = self.driver.count("vcard-card-fn")
        for i in range(count):
            if fn in self.driver.get_text("vcard-card-fn", index=i):
                self.driver.click("vcard-card", index=i)
                return
        if self.driver.count("vcard-card") > 0:
            self.driver.click("vcard-card")

    def detail_fn(self) -> str:
        return self.driver.get_text("vcard-detail-fn")

    def detail_emails(self) -> list[str]:
        count = self.driver.count("vcard-detail-email")
        return [self.driver.get_text("vcard-detail-email", index=i) for i in range(count)]

    def detail_tels(self) -> list[str]:
        count = self.driver.count("vcard-detail-tel")
        return [self.driver.get_text("vcard-detail-tel", index=i) for i in range(count)]

    def detail_addresses(self) -> list[str]:
        count = self.driver.count("vcard-detail-adr")
        return [self.driver.get_text("vcard-detail-adr", index=i) for i in range(count)]
