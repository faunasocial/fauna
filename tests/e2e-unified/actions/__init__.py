from __future__ import annotations

from typing import TYPE_CHECKING

from .auth import AuthActions
from .conversations import ConversationsActions
from .feed import FeedActions
from .contacts import ContactsActions
from .media import MediaActions
from .events import EventsActions
from .settings import SettingsActions
from .sync_locations import SyncLocationsActions
from .mail_settings import MailSettingsActions
from .atproto_settings import AtprotoSettingsActions
from .mail_aliases import MailAliasesActions
from .mail_lists import MailListsActions
from .mail_list_members import MailListMembersActions
from .muted_words import MutedWordsActions
from .web_settings import WebSettingsActions
from .labeler_catalog import LabelerCatalogActions
from .personalization import PersonalizationActions
from .linked_nests import LinkedNestsActions
from .custody import CustodyActions
from .nest_trust import NestTrustActions
from .nostr import NostrActions
from .p2p import P2PActions
from .task_delegation import TaskDelegationActions
from .connected_apps import ConnectedAppsActions
from .sessions import SessionsActions
from .logs import LogsActions
from .onboarding import OnboardingActions
from .admin import AdminActions
from .notifications import NotificationsActions
from .backups import BackupsActions
from .search import SearchActions
from .moderation import ModerationActions
from .report import ReportActions
from .subscriptions import SubscriptionsActions
from .profile import ProfileActions
from .family import FamilyActions
from .bridges import BridgesActions
from .retire import RetireActions
from .feature_policy_editor import FeaturePolicyEditorActions

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class ActionLayer:
    """Top-level API for tests. Composes all action groups."""

    def __init__(self, driver: PlatformDriver):
        self.driver = driver
        # Direct driver passthroughs used by snapshot-fixturing tests
        # (e.g. tests/test_handle_entry_outcomes.py) per the onboarding
        # client-target-state design (tracked internally).
        # These tests assert UI rendering against a fixtured wizard
        # snapshot, not action-layer flows, so the cleanest API is to
        # expose the predicates on the same `app` object the rest of
        # the suite uses.
        self.is_visible = driver.is_visible
        self.is_enabled = driver.is_enabled
        self.get_text = driver.get_text
        self.click = driver.click
        self.count = driver.count
        self.wait_for = driver.wait_for
        # Convention 14's causal anchor, exposed on `app` because it belongs in
        # the same reach as `wait_for`: a negative assert reads the observable,
        # triggers, awaits the trigger's own completion, then `app.barrier()`
        # before asserting nothing else happened. See `PlatformDriver.barrier`.
        self.barrier = driver.barrier
        self.auth = AuthActions(driver)
        self.conversations = ConversationsActions(driver)
        self.feed = FeedActions(driver)
        self.contacts = ContactsActions(driver)
        self.media = MediaActions(driver)
        self.events = EventsActions(driver)
        self.settings = SettingsActions(driver)
        self.sync_locations = SyncLocationsActions(driver)
        self.mail_settings = MailSettingsActions(driver)
        self.atproto_settings = AtprotoSettingsActions(driver)
        self.mail_aliases = MailAliasesActions(driver)
        self.mail_lists = MailListsActions(driver)
        self.mail_list_members = MailListMembersActions(driver)
        self.muted_words = MutedWordsActions(driver)
        self.web_settings = WebSettingsActions(driver)
        self.labeler_catalog = LabelerCatalogActions(driver)
        self.personalization = PersonalizationActions(driver)
        self.linked_nests = LinkedNestsActions(driver)
        self.custody = CustodyActions(driver)
        self.nest_trust = NestTrustActions(driver)
        self.nostr = NostrActions(driver)
        self.p2p = P2PActions(driver)
        self.task_delegation = TaskDelegationActions(driver)
        self.connected_apps = ConnectedAppsActions(driver)
        self.sessions = SessionsActions(driver)
        self.logs = LogsActions(driver)
        self.onboarding = OnboardingActions(driver)
        self.admin = AdminActions(driver)
        self.notifications = NotificationsActions(driver)
        self.backups = BackupsActions(driver)
        self.search = SearchActions(driver)
        self.moderation = ModerationActions(driver)
        self.report = ReportActions(driver)
        self.subscriptions = SubscriptionsActions(driver)
        self.profile = ProfileActions(driver)
        self.family = FamilyActions(driver)
        self.bridges = BridgesActions(driver)
        self.retire = RetireActions(driver)
        self.feature_policy_editor = FeaturePolicyEditorActions(driver)

    # --- Cross-group flows (need more than one action group) ---

    # (The offers re-read helper moved to `ProfileActions` once the Tiers-tab
    # re-read door was ruled uniform — it no longer needs the contacts
    # tap-through, so it is no longer a cross-group flow.)

    # --- Primitive driver delegations ---
    #
    # Per the E2E bridge contract (tracked internally): tests written
    # against the wizard pages
    # (test_handle_entry_outcomes.py, test_invite_request_states.py)
    # call `app.is_visible(...)` / `app.is_enabled(...)` / `app.get_text(...)`
    # / `app.click(...)` directly. Forward to driver.

    def is_visible(self, element_id: str, *, scope: str | None = None) -> bool:
        if scope is not None:
            return self.driver.is_visible(element_id, scope=scope)
        return self.driver.is_visible(element_id)

    def is_absent(self, element_id: str, *, scope: str | None = None) -> bool:
        """The honest NEGATIVE visibility read — `PlatformDriver.is_absent`. A
        method rather than an `__init__` passthrough like `is_visible` above, so
        building an `ActionLayer` never depends on the driver carrying it."""
        return self.driver.is_absent(element_id, scope=scope)

    def is_enabled(self, element_id: str, *, scope: str | None = None) -> bool:
        if scope is not None:
            return self.driver.is_enabled(element_id, scope=scope)
        return self.driver.is_enabled(element_id)

    def get_text(self, element_id: str, *, scope: str | None = None) -> str:
        if scope is not None:
            return self.driver.get_text(element_id, scope=scope)
        return self.driver.get_text(element_id)

    def click(self, element_id: str, *, scope: str | None = None) -> None:
        if scope is not None:
            self.driver.click(element_id, scope=scope)
        else:
            self.driver.click(element_id)

    def count(self, element_id: str, *, scope: str | None = None) -> int:
        if scope is not None:
            return self.driver.count(element_id, scope=scope)
        return self.driver.count(element_id)

    # --- Message reading (error / warning / info) ---
    #
    # Each method tries the state protocol first (messages.error, etc.)
    # and falls back to the UI element for clients that don't yet
    # serialize messages into state.

    def _message_from_state(self, level: str) -> str | None:
        """Try to read a message from the state protocol.

        Returns the message string, or None if the state protocol
        doesn't have a messages field (client hasn't implemented it yet).
        """
        try:
            value = self.driver.get_state(f"messages.{level}")
            if value is not None:
                return str(value)
            # Key existed but value was null -- no active message
            # Distinguish "key exists with null" from "no messages key at all"
            messages = self.driver.get_state("messages")
            if messages is not None and level in messages:
                return ""
        except Exception:
            pass
        return None

    def _message_from_element(self, element_id: str) -> str:
        """Fall back to reading a UI element directly."""
        if not self.driver.is_visible(element_id):
            return ""
        try:
            return self.driver.get_text(element_id)
        except Exception:
            return ""

    def error_text(self) -> str:
        """Read the current error message displayed on any page.

        Tries state protocol first (messages.error), falls back to the
        'error-message' UI element for backward compatibility. Returns
        empty string if no error is visible.
        """
        from_state = self._message_from_state("error")
        if from_state is not None:
            return from_state
        return self._message_from_element("error-message")

    def has_error(self) -> bool:
        """Check if an error message is currently visible."""
        from_state = self._message_from_state("error")
        if from_state is not None:
            return len(from_state) > 0
        return self.driver.is_visible("error-message")

    def warning_text(self) -> str:
        """Read the current warning message displayed on any page.

        Tries state protocol first (messages.warning), falls back to the
        'warning-message' UI element. Returns empty string if no warning.
        """
        from_state = self._message_from_state("warning")
        if from_state is not None:
            return from_state
        return self._message_from_element("warning-message")

    def has_warning(self) -> bool:
        """Check if a warning message is currently visible."""
        from_state = self._message_from_state("warning")
        if from_state is not None:
            return len(from_state) > 0
        return self.driver.is_visible("warning-message")

    def info_text(self) -> str:
        """Read the current info message displayed on any page.

        Tries state protocol first (messages.info), falls back to the
        'info-message' UI element. Returns empty string if no info message.
        """
        from_state = self._message_from_state("info")
        if from_state is not None:
            return from_state
        return self._message_from_element("info-message")

    def has_info(self) -> bool:
        """Check if an info message is currently visible."""
        from_state = self._message_from_state("info")
        if from_state is not None:
            return len(from_state) > 0
        return self.driver.is_visible("info-message")

    # --- State-based data readers ---
    #
    # These read from the state protocol, which works identically on all
    # 6 platforms (JSON over HTTP). Use these for assertions about app data.
    # Use UI element interactions (click, type_text) for write operations.
    #
    # (design decision, tracked internally)

    def feed_posts_from_state(self, min_count: int = 0,
                               timeout: float = 5.0) -> list[dict]:
        """Read feed posts from state protocol.

        Args:
            min_count: if > 0, polls until at least this many posts appear
                       (handles async data loading on native apps).
            timeout: max seconds to wait for min_count.

        Returns:
            List of post dicts with keys: post_id, author, body, timestamp,
            tags, has_media, is_reply. Returns [] if feed section is null
            (client hasn't implemented serialization).
        """
        def _extract(state):
            feed = (state or {}).get("data", {}).get("feed")
            if feed is None:
                return []
            return feed.get("posts", [])

        if min_count > 0:
            state = self.driver.get_state(
                wait_for=lambda s: len(_extract(s)) >= min_count,
                timeout=timeout,
            )
            return _extract(state)
        state = self.driver.get_state()
        return _extract(state)

    def contacts_from_state(self) -> list[dict]:
        """Read contacts from state protocol."""
        state = self.driver.get_state()
        contacts = (state or {}).get("data", {}).get("contacts")
        if contacts is None:
            return []
        return contacts

    def unread_count_from_state(self) -> int | None:
        """Read unread notification count from state.

        Returns None if notifications section is null (not implemented).
        Returns 0 if implemented but no unread.
        """
        state = self.driver.get_state()
        notif = (state or {}).get("data", {}).get("notifications")
        if notif is None:
            return None
        return notif.get("unread_count", 0)

    def inbox_mode_from_state(self) -> str | None:
        """Read current inbox mode from state settings.

        Returns None if settings section doesn't include inbox_mode.
        """
        state = self.driver.get_state()
        settings = (state or {}).get("settings")
        if settings is None:
            return None
        return settings.get("inbox_mode")
